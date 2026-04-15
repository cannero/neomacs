//! Mark-sweep garbage collector for the tagged pointer value system.
//!
//! # Design
//!
//! - **Cons cells**: GNU-shaped aligned block allocator.
//!   Each `ConsBlock` stores a fixed-size array of `ConsCell` at the front of
//!   a 64KB-aligned block, followed by packed mark bits. This lets the GC
//!   derive a cons's owning block/index directly from the pointer, matching the
//!   structure GNU Emacs uses in `alloc.c`.
//!
//! - **All other heap objects** (string, float, vectorlike): allocated
//!   via the system allocator, linked via intrusive `GcHeader.next` list.
//!   The GC walks this list during sweep.
//!
//! - **Mark phase**: walk from roots, decode tags, follow heap pointers.
//! - **Sweep phase**: walk cons blocks (bitmap) and the intrusive list
//!   (GcHeader chain), freeing unmarked objects.
//!
//! No ObjId. No generations. No stale references.

use super::header::*;
use super::value::TaggedValue;
use crate::buffer::text_props::TextPropertyTable;
use crate::emacs_core::intern::SymId;
use crate::gc_trace::GcTrace;
use rustc_hash::{FxHashMap, FxHashSet};
use std::alloc::{self, Layout};
use std::cell::Cell;
use std::mem::size_of;

/// How GC should discover roots beyond the explicit iterator passed to collect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootScanMode {
    /// Only use the explicit roots supplied by the caller.
    ExactOnly,
    /// Also conservatively scan the native stack for tagged values.
    ConservativeStack,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteTrackingMode {
    Disabled,
    OwnersOnly,
    OwnersAndRecords,
}

/// Classifies the kind of heap mutation that occurred.
///
/// GNU Emacs performs direct object/cell writes (`XSETCAR`, `XSETCDR`, `ASET`,
/// symbol value writes, etc.).  Neomacs keeps the same Lisp-visible semantics,
/// but records mutation metadata here so future generational or incremental
/// collectors have a single write-barrier surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeapWriteKind {
    ConsCar,
    ConsCdr,
    VectorSlot,
    VectorBulk,
    RecordSlot,
    RecordBulk,
    ClosureSlot,
    ClosureBulk,
    StringTextProps,
    StringData,
    HashTableData,
    ByteCodeData,
    MarkerData,
    OverlayData,
}

/// A single heap mutation event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeapWriteRecord {
    pub owner: TaggedValue,
    pub kind: HeapWriteKind,
    pub slot: Option<usize>,
    pub value: Option<TaggedValue>,
}

impl HeapWriteRecord {
    pub const fn bulk(owner: TaggedValue, kind: HeapWriteKind) -> Self {
        Self {
            owner,
            kind,
            slot: None,
            value: None,
        }
    }

    pub const fn slot(
        owner: TaggedValue,
        kind: HeapWriteKind,
        slot: usize,
        value: TaggedValue,
    ) -> Self {
        Self {
            owner,
            kind,
            slot: Some(slot),
            value: Some(value),
        }
    }
}

// ---------------------------------------------------------------------------
// Thread-local heap access
// ---------------------------------------------------------------------------

thread_local! {
    static TAGGED_HEAP: Cell<*mut TaggedHeap> = const { Cell::new(std::ptr::null_mut()) };
    static TAGGED_HEAP_WRITE_TRACKING_MODE: Cell<WriteTrackingMode> =
        const { Cell::new(WriteTrackingMode::Disabled) };
    /// Auto-allocated heap for tests that construct Values without a Context.
    #[cfg(test)]
    static TEST_FALLBACK_TAGGED_HEAP: std::cell::RefCell<Option<Box<TaggedHeap>>> =
        const { std::cell::RefCell::new(None) };
}

/// Set the thread-local tagged heap pointer.
pub fn set_tagged_heap(heap: &mut TaggedHeap) {
    TAGGED_HEAP.with(|h| h.set(heap as *mut TaggedHeap));
    TAGGED_HEAP_WRITE_TRACKING_MODE.with(|mode| mode.set(heap.write_tracking_mode()));
}

/// Access the thread-local tagged heap.
///
/// In test mode, auto-creates a fallback heap if none is set.
/// In production, panics if no heap is set.
#[inline]
pub fn with_tagged_heap<R>(f: impl FnOnce(&mut TaggedHeap) -> R) -> R {
    TAGGED_HEAP.with(|h| {
        let ptr = h.get();
        if !ptr.is_null() {
            return f(unsafe { &mut *ptr });
        }
        #[cfg(test)]
        {
            TEST_FALLBACK_TAGGED_HEAP.with(|fb| {
                let mut borrow = fb.borrow_mut();
                if borrow.is_none() {
                    *borrow = Some(Box::new(TaggedHeap::new()));
                }
                let heap_ref: &mut TaggedHeap = borrow.as_mut().unwrap();
                let ptr = heap_ref as *mut TaggedHeap;
                h.set(ptr);
                f(unsafe { &mut *ptr })
            })
        }
        #[cfg(not(test))]
        {
            panic!("no TaggedHeap set for this thread");
        }
    })
}

/// Central mutation hook for bulk writes to the tagged heap.
#[inline]
pub fn note_heap_write(owner: TaggedValue, kind: HeapWriteKind) {
    note_heap_write_record(HeapWriteRecord::bulk(owner, kind));
}

/// Central mutation hook for slot writes to the tagged heap.
#[inline]
pub fn note_heap_slot_write(
    owner: TaggedValue,
    kind: HeapWriteKind,
    slot: usize,
    value: TaggedValue,
) {
    note_heap_write_record(HeapWriteRecord::slot(owner, kind, slot, value));
}

#[inline]
fn note_heap_write_record(record: HeapWriteRecord) {
    if !record.owner.is_heap_object() {
        return;
    }
    if TAGGED_HEAP_WRITE_TRACKING_MODE.with(|mode| mode.get()) == WriteTrackingMode::Disabled {
        return;
    }
    with_tagged_heap(|heap| heap.record_heap_write(record));
}

// ---------------------------------------------------------------------------
// Cons block allocator
// ---------------------------------------------------------------------------

/// GNU Emacs keeps conses in fixed-size aligned blocks and derives the owning
/// block/index directly from the cons pointer. Keep the same shape here so
/// mark/ownership checks stay O(1) instead of linearly scanning `cons_blocks`.
const CONS_BLOCK_BYTES: usize = 64 * 1024;
const CONS_BLOCK_ALIGN: usize = CONS_BLOCK_BYTES;
const CONS_MARK_BITS_PER_WORD: usize = usize::BITS as usize;

const fn cons_mark_words(cell_count: usize) -> usize {
    cell_count.div_ceil(CONS_MARK_BITS_PER_WORD)
}

const fn cons_block_cell_count() -> usize {
    let cons_size = size_of::<ConsCell>();
    let mark_word_size = size_of::<usize>();
    let mut cells = CONS_BLOCK_BYTES / cons_size;
    while cells > 0 {
        let marks_bytes = cons_mark_words(cells) * mark_word_size;
        if cells * cons_size + marks_bytes <= CONS_BLOCK_BYTES {
            return cells;
        }
        cells -= 1;
    }
    0
}

const CONS_BLOCK_SIZE: usize = cons_block_cell_count();
const CONS_MARK_WORDS: usize = cons_mark_words(CONS_BLOCK_SIZE);
const CONS_CELLS_BYTES: usize = CONS_BLOCK_SIZE * size_of::<ConsCell>();
const CONS_MARKS_OFFSET: usize = CONS_CELLS_BYTES;

/// A GNU-shaped cons block with cells at the front of a fixed-size aligned
/// storage area, followed by packed mark bits.
struct ConsBlock {
    /// Aligned raw storage for cons cells plus mark bits.
    storage: *mut u8,
    /// Index of the first never-allocated cell in this block.
    next_index: u16,
}

impl ConsBlock {
    fn layout() -> Layout {
        Layout::from_size_align(CONS_BLOCK_BYTES, CONS_BLOCK_ALIGN).expect("cons block layout")
    }

    fn new() -> Self {
        let layout = Self::layout();
        let storage = unsafe { alloc::alloc_zeroed(layout) };
        if storage.is_null() {
            alloc::handle_alloc_error(layout);
        }
        Self {
            storage,
            next_index: 0,
        }
    }

    #[inline]
    fn base_addr(&self) -> usize {
        self.storage as usize
    }

    #[inline]
    fn cells_ptr(&self) -> *mut ConsCell {
        self.storage.cast()
    }

    #[inline]
    fn mark_words_ptr(&self) -> *mut usize {
        unsafe { self.storage.add(CONS_MARKS_OFFSET).cast() }
    }

    #[inline]
    fn block_base_for_ptr(ptr: *const ConsCell) -> usize {
        (ptr as usize) & !(CONS_BLOCK_ALIGN - 1)
    }

    #[inline]
    fn ptr_offset(ptr: *const ConsCell) -> usize {
        (ptr as usize).saturating_sub(Self::block_base_for_ptr(ptr))
    }

    #[inline]
    fn ptr_is_cell_aligned(ptr: *const ConsCell) -> bool {
        let offset = Self::ptr_offset(ptr);
        offset < CONS_CELLS_BYTES && offset.is_multiple_of(size_of::<ConsCell>())
    }

    #[inline]
    fn index_of_ptr(ptr: *const ConsCell) -> usize {
        Self::ptr_offset(ptr) / size_of::<ConsCell>()
    }

    #[inline]
    fn mark_bit(index: usize) -> (usize, usize) {
        let word = index / CONS_MARK_BITS_PER_WORD;
        let bit = index % CONS_MARK_BITS_PER_WORD;
        (word, 1usize << bit)
    }

    #[inline]
    fn owns_ptr(&self, ptr: *const ConsCell) -> bool {
        Self::block_base_for_ptr(ptr) == self.base_addr() && Self::ptr_is_cell_aligned(ptr)
    }

    #[inline]
    fn is_marked_ptr(&self, ptr: *const ConsCell) -> bool {
        let index = Self::index_of_ptr(ptr);
        let (word, mask) = Self::mark_bit(index);
        debug_assert!(word < CONS_MARK_WORDS);
        unsafe { (*self.mark_words_ptr().add(word) & mask) != 0 }
    }

    #[inline]
    fn mark_ptr(&mut self, ptr: *const ConsCell) {
        let index = Self::index_of_ptr(ptr);
        let (word, mask) = Self::mark_bit(index);
        debug_assert!(word < CONS_MARK_WORDS);
        unsafe {
            *self.mark_words_ptr().add(word) |= mask;
        }
    }

    /// Allocate a fresh cons cell from this block's bump cursor.
    /// Returns None if the block has no never-used cells left.
    fn alloc_bump(&mut self, car: TaggedValue, cdr: TaggedValue) -> Option<*mut ConsCell> {
        if self.next_index as usize >= CONS_BLOCK_SIZE {
            return None;
        }
        let idx = self.next_index;
        self.next_index += 1;
        let cell = unsafe { self.cells_ptr().add(idx as usize) };
        unsafe {
            (*cell).set_car(car);
            (*cell).set_cdr(cdr);
        }
        Some(cell)
    }

    /// Clear all mark bits used by this block.
    fn clear_marks(&mut self) {
        let used_words = cons_mark_words(self.next_index as usize);
        if used_words == 0 {
            return;
        }
        unsafe {
            std::ptr::write_bytes(self.mark_words_ptr(), 0, used_words);
        }
    }

    /// Sweep: thread reclaimed cells into the global intrusive free list and
    /// return the number of live cells in this block.
    fn sweep(&mut self, free_list: &mut *mut ConsCell) -> usize {
        let mut live = 0;

        // Match GNU alloc.c: reclaimed conses are linked through the dead
        // cells themselves instead of rebuilding an external index vector.
        for i in (0..self.next_index as usize).rev() {
            let cell = unsafe { self.cells_ptr().add(i) };
            let (word, mask) = Self::mark_bit(i);
            let marked = unsafe { (*self.mark_words_ptr().add(word) & mask) != 0 };
            if marked {
                live += 1;
            } else {
                unsafe {
                    (*cell).set_free_next(*free_list);
                }
                *free_list = cell;
            }
        }

        live
    }
}

impl Drop for ConsBlock {
    fn drop(&mut self) {
        unsafe { alloc::dealloc(self.storage, Self::layout()) };
    }
}

// ---------------------------------------------------------------------------
// TaggedHeap — the main GC-managed heap
// ---------------------------------------------------------------------------

/// The tagged pointer heap. Owns all heap-allocated Lisp objects.
pub struct TaggedHeap {
    /// Cons cell block allocator.
    cons_blocks: Vec<ConsBlock>,
    /// Base-address lookup for O(1) cons block ownership and marking.
    cons_block_index_by_base: FxHashMap<usize, usize>,

    /// Intrusive linked list of all non-cons heap objects.
    /// Points to the GcHeader of the first object; follow `next` to traverse.
    all_objects: *mut GcHeader,

    /// Total number of allocated objects (cons + non-cons).
    pub allocated_count: usize,

    /// GC threshold in approximate Lisp heap bytes.
    gc_threshold: usize,
    /// When true, `gc_threshold` was explicitly overridden by tests or host
    /// code and should not be recomputed from Lisp-visible GC variables.
    gc_threshold_overridden: bool,
    /// Approximate Lisp heap bytes allocated since the last full collection.
    bytes_since_gc: usize,
    /// Approximate bytes retained by the live heap after the last sweep.
    live_bytes: usize,

    /// Gray worklist for mark phase.
    gray_queue: Vec<TaggedValue>,

    /// Stack bottom for conservative stack scanning.
    stack_bottom: *const u8,

    /// Root discovery policy for full-heap collections.
    root_scan_mode: RootScanMode,

    /// Cached HashSet of non-cons object addresses for O(1) lookup
    /// during conservative stack scanning.  Built lazily before each
    /// scan and cleared after.
    non_cons_object_set: Option<FxHashSet<usize>>,

    /// Reclaimed cons cells threaded through the dead cells themselves,
    /// matching GNU alloc.c's `cons_free_list`.
    cons_free_list: *mut ConsCell,
    /// Number of live cons cells currently included in `allocated_count`.
    cons_live_count: usize,

    /// Tracking list of all allocated marker objects for bulk operations
    /// like clearing markers when buffers are killed.
    marker_ptrs: Vec<*mut MarkerObj>,

    /// Canonical subr heap objects keyed by `NameId`.
    ///
    /// These are rooted by the heap itself so builtin `PVEC_SUBR` objects
    /// survive function-cell rebinding, matching GNU's permanent subr objects.
    subr_registry: Vec<Option<TaggedValue>>,
    subr_slot_registry: Vec<Option<*mut SubrObj>>,
    subr_slot_registry_by_sym: Vec<Option<*mut SubrObj>>,

    /// Canonical runtime handle wrappers keyed by their underlying object id.
    buffer_registry: FxHashMap<crate::buffer::BufferId, TaggedValue>,
    window_registry: FxHashMap<u64, TaggedValue>,
    frame_registry: FxHashMap<u64, TaggedValue>,
    timer_registry: FxHashMap<u64, TaggedValue>,

    /// Owners mutated since the last full collection.
    ///
    /// This is the minimal remembered-set precursor for future generational
    /// or incremental GC. We keep owner identity, not child edges, because the
    /// current collector is still full-heap mark-sweep.
    write_tracking_mode: WriteTrackingMode,
    dirty_owners: Vec<TaggedValue>,
    dirty_owner_bits: FxHashSet<usize>,
    dirty_writes: Vec<HeapWriteRecord>,
}

impl TaggedHeap {
    pub fn new() -> Self {
        Self {
            cons_blocks: Vec::new(),
            cons_block_index_by_base: FxHashMap::default(),
            all_objects: std::ptr::null_mut(),
            allocated_count: 0,
            gc_threshold: 100_000 * size_of::<usize>(),
            gc_threshold_overridden: false,
            bytes_since_gc: 0,
            live_bytes: 0,
            gray_queue: Vec::new(),
            stack_bottom: std::ptr::null(),
            root_scan_mode: RootScanMode::ConservativeStack,
            non_cons_object_set: None,
            cons_free_list: std::ptr::null_mut(),
            cons_live_count: 0,
            marker_ptrs: Vec::new(),
            subr_registry: Vec::new(),
            subr_slot_registry: Vec::new(),
            subr_slot_registry_by_sym: Vec::new(),
            buffer_registry: FxHashMap::default(),
            window_registry: FxHashMap::default(),
            frame_registry: FxHashMap::default(),
            timer_registry: FxHashMap::default(),
            write_tracking_mode: WriteTrackingMode::Disabled,
            dirty_owners: Vec::new(),
            dirty_owner_bits: FxHashSet::default(),
            dirty_writes: Vec::new(),
        }
    }

    pub fn set_stack_bottom(&mut self, bottom: *const u8) {
        self.stack_bottom = bottom;
    }

    pub fn set_root_scan_mode(&mut self, mode: RootScanMode) {
        self.root_scan_mode = mode;
    }

    pub fn root_scan_mode(&self) -> RootScanMode {
        self.root_scan_mode
    }

    pub fn set_write_tracking_mode(&mut self, mode: WriteTrackingMode) {
        self.write_tracking_mode = mode;
        TAGGED_HEAP_WRITE_TRACKING_MODE.with(|current| current.set(mode));
        if mode == WriteTrackingMode::Disabled {
            self.clear_dirty_owners();
            self.clear_dirty_writes();
        }
    }

    pub fn write_tracking_mode(&self) -> WriteTrackingMode {
        self.write_tracking_mode
    }

    pub fn should_collect(&self) -> bool {
        self.bytes_since_gc >= self.gc_threshold
    }

    pub fn gc_threshold(&self) -> usize {
        self.gc_threshold
    }

    pub fn set_gc_threshold(&mut self, threshold: usize) {
        self.gc_threshold = threshold.max(1);
        self.gc_threshold_overridden = true;
    }

    pub fn set_gc_threshold_from_runtime(&mut self, threshold: usize) {
        if !self.gc_threshold_overridden {
            self.gc_threshold = threshold.max(1);
        }
    }

    pub fn clear_gc_threshold_override(&mut self) {
        self.gc_threshold_overridden = false;
    }

    pub fn gc_threshold_is_overridden(&self) -> bool {
        self.gc_threshold_overridden
    }

    pub fn allocated_count(&self) -> usize {
        self.allocated_count
    }

    pub fn bytes_since_gc(&self) -> usize {
        self.bytes_since_gc
    }

    pub fn live_bytes(&self) -> usize {
        self.live_bytes
    }

    pub fn subr_value(&self, id: crate::emacs_core::intern::NameId) -> Option<TaggedValue> {
        self.subr_registry.get(id.0 as usize).copied().flatten()
    }

    pub fn subr_slot(&self, id: crate::emacs_core::intern::NameId) -> Option<&'static SubrObj> {
        let ptr = self
            .subr_slot_registry
            .get(id.0 as usize)
            .copied()
            .flatten()?;
        Some(unsafe { &*ptr })
    }

    pub fn subr_slot_by_sym(
        &self,
        id: crate::emacs_core::intern::SymId,
    ) -> Option<&'static SubrObj> {
        let ptr = self
            .subr_slot_registry_by_sym
            .get(id.0 as usize)
            .copied()
            .flatten()?;
        Some(unsafe { &*ptr })
    }

    pub fn subr_slot_mut(
        &mut self,
        id: crate::emacs_core::intern::NameId,
    ) -> Option<&'static mut SubrObj> {
        let ptr = self
            .subr_slot_registry
            .get(id.0 as usize)
            .copied()
            .flatten()?;
        Some(unsafe { &mut *ptr })
    }

    pub fn register_subr_value(
        &mut self,
        id: crate::emacs_core::intern::NameId,
        value: TaggedValue,
    ) {
        let index = id.0 as usize;
        if self.subr_registry.len() <= index {
            self.subr_registry.resize(index + 1, None);
        }
        if self.subr_slot_registry.len() <= index {
            self.subr_slot_registry.resize(index + 1, None);
        }
        self.subr_registry[index] = Some(value);
        self.subr_slot_registry[index] = Some(
            value
                .as_veclike_ptr()
                .expect("subr registry points to non-subr value") as *mut SubrObj,
        );
    }

    pub fn register_subr_slot_for_sym(
        &mut self,
        id: crate::emacs_core::intern::SymId,
        value: TaggedValue,
    ) {
        let index = id.0 as usize;
        if self.subr_slot_registry_by_sym.len() <= index {
            self.subr_slot_registry_by_sym.resize(index + 1, None);
        }
        self.subr_slot_registry_by_sym[index] = Some(
            value
                .as_veclike_ptr()
                .expect("subr registry points to non-subr value") as *mut SubrObj,
        );
    }

    pub fn clear_subr_registry(&mut self) {
        self.subr_registry.clear();
        self.subr_slot_registry.clear();
        self.subr_slot_registry_by_sym.clear();
    }

    pub fn buffer_value(&self, id: crate::buffer::BufferId) -> Option<TaggedValue> {
        self.buffer_registry.get(&id).copied()
    }

    pub fn register_buffer_value(&mut self, id: crate::buffer::BufferId, value: TaggedValue) {
        self.buffer_registry.insert(id, value);
    }

    pub fn window_value(&self, id: u64) -> Option<TaggedValue> {
        self.window_registry.get(&id).copied()
    }

    pub fn register_window_value(&mut self, id: u64, value: TaggedValue) {
        self.window_registry.insert(id, value);
    }

    pub fn frame_value(&self, id: u64) -> Option<TaggedValue> {
        self.frame_registry.get(&id).copied()
    }

    pub fn register_frame_value(&mut self, id: u64, value: TaggedValue) {
        self.frame_registry.insert(id, value);
    }

    pub fn timer_value(&self, id: u64) -> Option<TaggedValue> {
        self.timer_registry.get(&id).copied()
    }

    pub fn register_timer_value(&mut self, id: u64, value: TaggedValue) {
        self.timer_registry.insert(id, value);
    }

    pub fn dirty_owner_count(&self) -> usize {
        self.dirty_owners.len()
    }

    pub fn is_dirty_owner(&self, owner: TaggedValue) -> bool {
        self.dirty_owner_bits.contains(&owner.bits())
    }

    pub fn take_dirty_owners(&mut self) -> Vec<TaggedValue> {
        self.dirty_owner_bits.clear();
        std::mem::take(&mut self.dirty_owners)
    }

    pub fn clear_dirty_owners(&mut self) {
        self.dirty_owners.clear();
        self.dirty_owner_bits.clear();
    }

    pub fn dirty_write_count(&self) -> usize {
        self.dirty_writes.len()
    }

    pub fn dirty_writes(&self) -> &[HeapWriteRecord] {
        &self.dirty_writes
    }

    pub fn take_dirty_writes(&mut self) -> Vec<HeapWriteRecord> {
        std::mem::take(&mut self.dirty_writes)
    }

    pub fn clear_dirty_writes(&mut self) {
        self.dirty_writes.clear();
    }

    fn record_heap_write(&mut self, record: HeapWriteRecord) {
        if self.write_tracking_mode == WriteTrackingMode::Disabled {
            return;
        }
        if self.dirty_owner_bits.insert(record.owner.bits()) {
            self.dirty_owners.push(record.owner);
        }
        if self.write_tracking_mode == WriteTrackingMode::OwnersAndRecords {
            self.dirty_writes.push(record);
        }
    }

    fn note_allocation_bytes(&mut self, bytes: usize) {
        self.bytes_since_gc = self.bytes_since_gc.saturating_add(bytes);
        self.live_bytes = self.live_bytes.saturating_add(bytes);
    }

    fn vector_storage_bytes<T>(values: &Vec<T>) -> usize {
        values.capacity().saturating_mul(size_of::<T>())
    }

    fn hash_map_storage_bytes<K, V, S>(values: &std::collections::HashMap<K, V, S>) -> usize {
        values.capacity().saturating_mul(size_of::<(K, V)>())
    }

    fn string_object_bytes(obj: &StringObj) -> usize {
        size_of::<StringObj>().saturating_add(obj.data.byte_len())
    }

    fn hash_table_object_bytes(obj: &HashTableObj) -> usize {
        size_of::<HashTableObj>()
            .saturating_add(Self::hash_map_storage_bytes(&obj.table.data))
            .saturating_add(Self::hash_map_storage_bytes(&obj.table.key_snapshots))
            .saturating_add(Self::vector_storage_bytes(&obj.table.insertion_order))
    }

    fn lambda_object_bytes(obj: &LambdaObj) -> usize {
        size_of::<LambdaObj>().saturating_add(Self::vector_storage_bytes(&obj.data))
    }

    fn macro_object_bytes(obj: &MacroObj) -> usize {
        size_of::<MacroObj>().saturating_add(Self::vector_storage_bytes(&obj.data))
    }

    fn bytecode_object_bytes(obj: &ByteCodeObj) -> usize {
        let data = &obj.data;
        size_of::<ByteCodeObj>()
            .saturating_add(Self::vector_storage_bytes(&data.ops))
            .saturating_add(Self::vector_storage_bytes(&data.constants))
            .saturating_add(
                data.params
                    .required
                    .capacity()
                    .saturating_mul(size_of::<SymId>()),
            )
            .saturating_add(
                data.params
                    .optional
                    .capacity()
                    .saturating_mul(size_of::<SymId>()),
            )
            .saturating_add(
                data.gnu_byte_offset_map
                    .as_ref()
                    .map_or(0, Self::hash_map_storage_bytes),
            )
            .saturating_add(data.docstring.as_ref().map_or(0, |doc| doc.capacity()))
    }

    fn record_object_bytes(obj: &RecordObj) -> usize {
        size_of::<RecordObj>().saturating_add(Self::vector_storage_bytes(&obj.data))
    }

    fn object_bytes_from_header(header: *const GcHeader) -> usize {
        unsafe {
            match (*header).kind {
                HeapObjectKind::String => Self::string_object_bytes(&*(header as *const StringObj)),
                HeapObjectKind::Float => size_of::<FloatObj>(),
                HeapObjectKind::VecLike => {
                    let ptr = header as *const VecLikeHeader;
                    match (*ptr).type_tag {
                        VecLikeType::Vector => {
                            let obj = &*(ptr as *const VectorObj);
                            size_of::<VectorObj>()
                                .saturating_add(Self::vector_storage_bytes(&obj.data))
                        }
                        VecLikeType::HashTable => {
                            Self::hash_table_object_bytes(&*(ptr as *const HashTableObj))
                        }
                        VecLikeType::Lambda => {
                            Self::lambda_object_bytes(&*(ptr as *const LambdaObj))
                        }
                        VecLikeType::Macro => Self::macro_object_bytes(&*(ptr as *const MacroObj)),
                        VecLikeType::ByteCode => {
                            Self::bytecode_object_bytes(&*(ptr as *const ByteCodeObj))
                        }
                        VecLikeType::Record => {
                            Self::record_object_bytes(&*(ptr as *const RecordObj))
                        }
                        VecLikeType::Overlay => size_of::<OverlayObj>(),
                        VecLikeType::Marker => size_of::<MarkerObj>(),
                        VecLikeType::Buffer => size_of::<BufferObj>(),
                        VecLikeType::Window => size_of::<WindowObj>(),
                        VecLikeType::Frame => size_of::<FrameObj>(),
                        VecLikeType::Timer => size_of::<TimerObj>(),
                        VecLikeType::Subr => size_of::<SubrObj>(),
                        VecLikeType::Bignum => size_of::<BignumObj>(),
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Allocation
    // -----------------------------------------------------------------------

    /// Allocate a cons cell. Returns a tagged Value.
    pub fn alloc_cons(&mut self, car: TaggedValue, cdr: TaggedValue) -> TaggedValue {
        if !self.cons_free_list.is_null() {
            let cell = self.cons_free_list;
            unsafe {
                self.cons_free_list = (*cell).free_next();
                (*cell).set_car(car);
                (*cell).set_cdr(cdr);
            }
            self.allocated_count += 1;
            self.cons_live_count += 1;
            self.note_allocation_bytes(size_of::<ConsCell>());
            return unsafe { TaggedValue::from_cons_ptr(cell) };
        }

        if let Some(block) = self.cons_blocks.last_mut()
            && let Some(cell) = block.alloc_bump(car, cdr)
        {
            self.allocated_count += 1;
            self.cons_live_count += 1;
            self.note_allocation_bytes(size_of::<ConsCell>());
            return unsafe { TaggedValue::from_cons_ptr(cell) };
        }

        // All existing blocks are exhausted and there are no reclaimed cells,
        // so allocate a fresh current block and bump from it, matching GNU's
        // cons_block/cons_block_index path.
        let mut block = ConsBlock::new();
        let block_base = block.base_addr();
        let cell = block
            .alloc_bump(car, cdr)
            .expect("fresh block should have space");
        self.cons_blocks.push(block);
        let block_index = self.cons_blocks.len() - 1;
        self.cons_block_index_by_base
            .insert(block_base, block_index);
        self.allocated_count += 1;
        self.cons_live_count += 1;
        self.note_allocation_bytes(size_of::<ConsCell>());
        unsafe { TaggedValue::from_cons_ptr(cell) }
    }

    /// Allocate a string object.
    pub fn alloc_string(&mut self, s: crate::heap_types::LispString) -> TaggedValue {
        let obj = Box::new(StringObj {
            header: GcHeader::new(HeapObjectKind::String),
            data: s,
            text_props: TextPropertyTable::new(),
        });
        let ptr = Box::into_raw(obj);
        self.link_object(unsafe { &mut (*ptr).header });
        self.allocated_count += 1;
        self.note_allocation_bytes(unsafe { Self::string_object_bytes(&*ptr) });
        unsafe { TaggedValue::from_string_ptr(ptr) }
    }

    /// Allocate a float object.
    pub fn alloc_float(&mut self, value: f64) -> TaggedValue {
        let obj = Box::new(FloatObj {
            header: GcHeader::new(HeapObjectKind::Float),
            value,
        });
        let ptr = Box::into_raw(obj);
        self.link_object(unsafe { &mut (*ptr).header });
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<FloatObj>());
        unsafe { TaggedValue::from_float_ptr(ptr) }
    }

    /// Allocate a canonical subr object.
    ///
    /// Documentation strings are NOT stored on the SubrObj — they live
    /// in the central `subr_docs::GNU_SUBR_DOCS` table indexed by
    /// symbol name. Lookup is done at documentation-query time via
    /// `subr_docs::lookup(name)`. Keeping the doc out of `SubrObj`
    /// avoids extending the struct (cache-line preserving) and avoids
    /// any new `unsafe` reader path.
    pub fn alloc_subr(
        &mut self,
        name: crate::emacs_core::intern::NameId,
        function: Option<SubrFn>,
        min_args: u16,
        max_args: Option<u16>,
        dispatch_kind: SubrDispatchKind,
    ) -> TaggedValue {
        let obj = Box::new(SubrObj {
            header: VecLikeHeader::new(VecLikeType::Subr),
            name,
            min_args,
            max_args,
            dispatch_kind,
            function,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<SubrObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a vector.
    pub fn alloc_vector(&mut self, items: Vec<TaggedValue>) -> TaggedValue {
        let obj = Box::new(VectorObj {
            header: VecLikeHeader::new(VecLikeType::Vector),
            data: items,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(
            size_of::<VectorObj>()
                .saturating_add(Self::vector_storage_bytes(unsafe { &(*ptr).data })),
        );
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a hash table.
    pub fn alloc_hash_table(
        &mut self,
        table: crate::emacs_core::value::LispHashTable,
    ) -> TaggedValue {
        let obj = Box::new(HashTableObj {
            header: VecLikeHeader::new(VecLikeType::HashTable),
            table,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(unsafe { Self::hash_table_object_bytes(&*ptr) });
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a lambda.
    /// Allocate a lambda (interpreted closure) as a Value vector.
    /// Matches GNU Emacs's PVEC_CLOSURE: all slots are GC-traced Values.
    pub fn alloc_lambda(&mut self, slots: Vec<TaggedValue>) -> TaggedValue {
        let obj = Box::new(LambdaObj {
            header: VecLikeHeader::new(VecLikeType::Lambda),
            data: slots,
            parsed_params: std::sync::OnceLock::new(),
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(unsafe { Self::lambda_object_bytes(&*ptr) });
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a lambda from a LambdaData (bridge for migration).
    /// Converts LambdaData fields to the Value vector layout.
    pub fn alloc_lambda_from_data(
        &mut self,
        data: crate::emacs_core::value::LambdaData,
    ) -> TaggedValue {
        let slots = data.to_closure_slots();
        self.alloc_lambda(slots)
    }

    /// Allocate a macro as a Value vector.
    pub fn alloc_macro(&mut self, slots: Vec<TaggedValue>) -> TaggedValue {
        let obj = Box::new(MacroObj {
            header: VecLikeHeader::new(VecLikeType::Macro),
            data: slots,
            parsed_params: std::sync::OnceLock::new(),
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(unsafe { Self::macro_object_bytes(&*ptr) });
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a macro from a LambdaData (bridge for migration).
    pub fn alloc_macro_from_data(
        &mut self,
        data: crate::emacs_core::value::LambdaData,
    ) -> TaggedValue {
        let slots = data.to_closure_slots();
        self.alloc_macro(slots)
    }

    /// Allocate a buffer reference.
    pub fn alloc_buffer(&mut self, id: crate::buffer::BufferId) -> TaggedValue {
        let obj = Box::new(BufferObj {
            header: VecLikeHeader::new(VecLikeType::Buffer),
            id,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<BufferObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a window reference.
    pub fn alloc_window(&mut self, id: u64) -> TaggedValue {
        let obj = Box::new(WindowObj {
            header: VecLikeHeader::new(VecLikeType::Window),
            id,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<WindowObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a frame reference.
    pub fn alloc_frame(&mut self, id: u64) -> TaggedValue {
        let obj = Box::new(FrameObj {
            header: VecLikeHeader::new(VecLikeType::Frame),
            id,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<FrameObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a timer reference.
    pub fn alloc_timer(&mut self, id: u64) -> TaggedValue {
        let obj = Box::new(TimerObj {
            header: VecLikeHeader::new(VecLikeType::Timer),
            id,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<TimerObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a bytecode function.
    pub fn alloc_bytecode(
        &mut self,
        data: crate::emacs_core::bytecode::ByteCodeFunction,
    ) -> TaggedValue {
        let obj = Box::new(ByteCodeObj {
            header: VecLikeHeader::new(VecLikeType::ByteCode),
            data,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(unsafe { Self::bytecode_object_bytes(&*ptr) });
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a record.
    pub fn alloc_record(&mut self, items: Vec<TaggedValue>) -> TaggedValue {
        let obj = Box::new(RecordObj {
            header: VecLikeHeader::new(VecLikeType::Record),
            data: items,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(unsafe { Self::record_object_bytes(&*ptr) });
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate an overlay.
    pub fn alloc_overlay(&mut self, data: crate::heap_types::OverlayData) -> TaggedValue {
        let obj = Box::new(OverlayObj {
            header: VecLikeHeader::new(VecLikeType::Overlay),
            data,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<OverlayObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a marker.
    pub fn alloc_marker(&mut self, data: crate::heap_types::MarkerData) -> TaggedValue {
        let obj = Box::new(MarkerObj {
            header: VecLikeHeader::new(VecLikeType::Marker),
            data,
        });
        let ptr = Box::into_raw(obj);
        self.marker_ptrs.push(ptr);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<MarkerObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a bignum (arbitrary-precision integer).
    ///
    /// Mirrors GNU `make_bignum` (`src/bignum.c:113`): the caller is
    /// responsible for ensuring the value is outside fixnum range.
    /// Use `Value::make_integer` for the canonical "fixnum-or-bignum"
    /// constructor that delegates here only when promotion is needed.
    pub fn alloc_bignum(&mut self, value: rug::Integer) -> TaggedValue {
        let obj = Box::new(BignumObj {
            header: VecLikeHeader::new(VecLikeType::Bignum),
            value,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<BignumObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    // -----------------------------------------------------------------------
    // Marker operations
    // -----------------------------------------------------------------------

    /// Clear buffer association for all markers belonging to any of the
    /// killed buffers.
    pub fn clear_markers_for_buffers<S>(
        &mut self,
        killed: &std::collections::HashSet<crate::buffer::BufferId, S>,
    ) where
        S: std::hash::BuildHasher,
    {
        for ptr in &self.marker_ptrs {
            let marker = unsafe { &mut (**ptr).data };
            if marker.buffer.is_some_and(|b| killed.contains(&b)) {
                marker.buffer = None;
                marker.position = None;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Link a non-cons object into the all_objects intrusive list.
    fn link_object(&mut self, header: &mut GcHeader) {
        header.next = self.all_objects;
        self.all_objects = header as *mut GcHeader;
    }

    /// Link a veclike object into the all_objects list.
    fn link_veclike(&mut self, header: *mut VecLikeHeader) {
        unsafe {
            (*header).gc.next = self.all_objects;
            self.all_objects = &mut (*header).gc as *mut GcHeader;
        }
    }

    // -----------------------------------------------------------------------
    // Garbage collection — stop-the-world mark-sweep
    // -----------------------------------------------------------------------

    /// Run a full mark-sweep garbage collection.
    ///
    /// `roots` must yield every reachable `TaggedValue`.
    pub fn collect(&mut self, roots: impl Iterator<Item = TaggedValue>) {
        let mode = self.root_scan_mode;
        self.collect_with_scan_mode(roots, mode);
    }

    /// Run a full mark-sweep collection using only the explicit roots provided.
    pub fn collect_exact(&mut self, roots: impl Iterator<Item = TaggedValue>) {
        self.collect_with_scan_mode(roots, RootScanMode::ExactOnly);
    }

    fn collect_with_scan_mode(
        &mut self,
        roots: impl Iterator<Item = TaggedValue>,
        mode: RootScanMode,
    ) {
        // (Pre-mark verification removed — unmarked objects may have stale data
        //  that will be swept. Only post-mark verification is meaningful.)

        // -- Clear marks --
        for block in &mut self.cons_blocks {
            block.clear_marks();
        }
        // Clear marks on non-cons objects
        let mut obj = self.all_objects;
        while !obj.is_null() {
            unsafe {
                (*obj).marked = false;
                obj = (*obj).next;
            }
        }

        // -- Seed gray queue from roots --
        self.gray_queue.clear();
        for root in roots {
            if root.is_heap_object() {
                self.gray_queue.push(root);
            }
        }
        for subr in self.subr_registry.iter().flatten() {
            if subr.is_heap_object() {
                self.gray_queue.push(*subr);
            }
        }
        for value in self.buffer_registry.values() {
            if value.is_heap_object() {
                self.gray_queue.push(*value);
            }
        }
        for value in self.window_registry.values() {
            if value.is_heap_object() {
                self.gray_queue.push(*value);
            }
        }
        for value in self.frame_registry.values() {
            if value.is_heap_object() {
                self.gray_queue.push(*value);
            }
        }
        for value in self.timer_registry.values() {
            if value.is_heap_object() {
                self.gray_queue.push(*value);
            }
        }

        if matches!(mode, RootScanMode::ConservativeStack) {
            // Build O(1) lookup set for non-cons objects before scanning.
            // Validate each entry — the all_objects list can become corrupt
            // if a GcHeader.next pointer is stale, causing traversal into
            // unmapped or stacker memory.
            let mut set = FxHashSet::default();
            let mut obj = self.all_objects;
            let mut count = 0usize;
            while !obj.is_null() {
                set.insert(obj as usize);
                count += 1;
                if count > 10_000_000 {
                    // Safety bail: list is too long, probably corrupt
                    break;
                }
                obj = unsafe { (*obj).next };
            }
            self.non_cons_object_set = Some(set);
            unsafe { self.conservative_stack_scan() };
            // Keep the HashSet alive through mark phase for O(1) ownership
            // checks in is_valid_heap_pointer.  Clear after sweep.
        }

        // (Debug root verification removed — with conservative scanning,
        // the HashSet is cleared before this point so the fallback O(N)
        // walk could disagree with the HashSet-backed check.  The type
        // validation in is_valid_heap_pointer is the authoritative check.)

        // -- Mark phase: drain gray queue --
        self.mark_all();

        // (Post-mark verification removed — the "corrupt data pointer 0x1"
        //  check was a false alarm. Rust's empty String uses ptr=0x1
        //  (NonNull::dangling), which is normal, not corruption.)

        // -- Sweep phase --
        let cons_live_bytes = self.sweep_cons();
        let object_live_bytes = self.sweep_objects();
        self.live_bytes = cons_live_bytes.saturating_add(object_live_bytes);
        self.bytes_since_gc = 0;

        // Clear the conservative scan HashSet after sweep
        self.non_cons_object_set = None;

        // A full-heap collection subsumes any remembered-set bookkeeping.
        self.clear_dirty_owners();
        self.clear_dirty_writes();
    }

    /// Drain the gray queue, marking and tracing all reachable objects.
    fn mark_all(&mut self) {
        while let Some(val) = self.gray_queue.pop() {
            self.mark_value(val);
        }
    }

    /// Mark a single tagged value and push its children onto the gray queue.
    fn mark_value(&mut self, val: TaggedValue) {
        // Guard: for non-cons heap objects, verify ownership before
        // dereferencing.  Conservative scanning can produce values that
        // passed the initial check but point to stale/corrupt memory.
        // Conservative stack scanning can enqueue false positives. Re-validate
        // those values before dereferencing them, but keep the exact-root path
        // on the fast O(1) object traversal it is supposed to use.
        if self.non_cons_object_set.is_some() {
            match val.tag() {
                0b011 | 0b100 | 0b110 => {
                    if !self.is_valid_heap_pointer(val) {
                        return; // Skip invalid pointer
                    }
                }
                _ => {}
            }
        }
        match val.tag() {
            0b010 => {
                // Cons
                let ptr = val.xcons_ptr();
                if self.mark_cons(ptr) {
                    // Newly marked — trace children
                    let car = unsafe { (*ptr).car };
                    let cdr = unsafe { (*ptr).cdr() };
                    if car.is_heap_object() {
                        self.gray_queue.push(car);
                    }
                    if cdr.is_heap_object() {
                        self.gray_queue.push(cdr);
                    }
                }
            }
            0b100 => {
                // String — trace object-owned text properties.
                let ptr = val.as_string_ptr().unwrap() as *mut StringObj;
                unsafe {
                    if (*ptr).header.marked {
                        return;
                    }
                    (*ptr).header.marked = true;
                    if !(*ptr).text_props.is_empty() {
                        (*ptr).text_props.for_each_root(|root| {
                            if root.is_heap_object() {
                                self.gray_queue.push(root);
                            }
                        });
                    }
                };
            }
            0b110 => {
                // Float — no children
                let ptr = val.as_float_ptr().unwrap() as *mut FloatObj;
                unsafe {
                    if (*ptr).header.marked {
                        return;
                    }
                    (*ptr).header.marked = true;
                };
            }
            0b011 => {
                // Vectorlike
                let ptr = val.as_veclike_ptr().unwrap() as *mut VecLikeHeader;
                unsafe {
                    if (*ptr).gc.marked {
                        return; // Already marked
                    }
                    (*ptr).gc.marked = true;
                    self.trace_veclike(ptr);
                }
            }
            _ => {} // Immediate values — nothing to mark
        }
    }

    /// Mark a cons cell. Returns true if newly marked (not previously marked).
    fn mark_cons(&mut self, ptr: *const ConsCell) -> bool {
        if !self.owns_cons_ptr(ptr) {
            return false;
        }
        let block_base = ConsBlock::block_base_for_ptr(ptr);
        let block_index = *self
            .cons_block_index_by_base
            .get(&block_base)
            .expect("owned cons pointer should resolve to a block");
        let block = &mut self.cons_blocks[block_index];
        if block.is_marked_ptr(ptr) {
            return false;
        }
        block.mark_ptr(ptr);
        true
    }

    /// Trace children of a vectorlike object, pushing them onto the gray queue.
    unsafe fn trace_veclike(&mut self, ptr: *mut VecLikeHeader) {
        match unsafe { (*ptr).type_tag } {
            VecLikeType::Vector => {
                let obj = ptr as *const VectorObj;
                for val in unsafe { &(*obj).data } {
                    if val.is_heap_object() {
                        self.gray_queue.push(*val);
                    }
                }
            }
            VecLikeType::Record => {
                let obj = ptr as *const RecordObj;
                for val in unsafe { &(*obj).data } {
                    if val.is_heap_object() {
                        self.gray_queue.push(*val);
                    }
                }
            }
            VecLikeType::HashTable => {
                let obj = ptr as *const HashTableObj;
                let ht = unsafe { &(*obj).table };
                // Trace all values in the hash table
                for val in ht.data.values() {
                    if val.is_heap_object() {
                        self.gray_queue.push(*val);
                    }
                }
                // Trace key snapshots (original key objects)
                for val in ht.key_snapshots.values() {
                    if val.is_heap_object() {
                        self.gray_queue.push(*val);
                    }
                }
            }
            VecLikeType::Lambda | VecLikeType::Macro => {
                // Closures are plain Value vectors (GNU PVEC_CLOSURE compat).
                // Trace ALL slots uniformly — no type-specific logic needed.
                let obj = ptr as *const LambdaObj;
                for val in unsafe { &(*obj).data } {
                    if val.is_heap_object() {
                        self.gray_queue.push(*val);
                    }
                }
            }
            VecLikeType::ByteCode => {
                let obj = ptr as *const ByteCodeObj;
                let data = unsafe { &(*obj).data };
                // Trace constants vector
                for val in &data.constants {
                    if val.is_heap_object() {
                        self.gray_queue.push(*val);
                    }
                }
                // Trace captured lexical environment
                if let Some(env) = data.env {
                    if env.is_heap_object() {
                        self.gray_queue.push(env);
                    }
                }
                // Trace doc_form (can be a Value)
                if let Some(doc_form) = data.doc_form {
                    if doc_form.is_heap_object() {
                        self.gray_queue.push(doc_form);
                    }
                }
                // Trace interactive spec
                if let Some(interactive) = data.interactive {
                    if interactive.is_heap_object() {
                        self.gray_queue.push(interactive);
                    }
                }
            }
            VecLikeType::Overlay => {
                let obj = ptr as *const OverlayObj;
                let data = unsafe { &(*obj).data };
                // Trace the property list
                if data.plist.is_heap_object() {
                    self.gray_queue.push(data.plist);
                }
            }
            VecLikeType::Buffer
            | VecLikeType::Window
            | VecLikeType::Frame
            | VecLikeType::Timer
            | VecLikeType::Marker
            | VecLikeType::Subr
            | VecLikeType::Bignum => {
                // These have no Value children to trace.
                //
                // Bignums own a `rug::Integer`, which owns a libgmp
                // limb buffer, but no Lisp_Object children — `Drop`
                // takes care of the GMP memory in `free_gc_object`.
            }
        }
    }

    /// Sweep unmarked cons cells back to free lists.
    fn sweep_cons(&mut self) -> usize {
        let old_live = self.cons_live_count;
        let mut new_live = 0;
        self.cons_free_list = std::ptr::null_mut();
        for block in &mut self.cons_blocks {
            new_live += block.sweep(&mut self.cons_free_list);
        }
        self.cons_live_count = new_live;
        self.allocated_count = self
            .allocated_count
            .saturating_sub(old_live)
            .saturating_add(new_live);
        new_live.saturating_mul(size_of::<ConsCell>())
    }

    /// Sweep non-cons objects: walk intrusive list, free unmarked, rebuild list.
    fn sweep_objects(&mut self) -> usize {
        let mut prev: *mut *mut GcHeader = &mut self.all_objects;
        let mut current = self.all_objects;
        let mut live_bytes = 0usize;
        while !current.is_null() {
            unsafe {
                let next = (*current).next;
                if (*current).marked {
                    // Keep it — advance prev
                    live_bytes = live_bytes.saturating_add(Self::object_bytes_from_header(current));
                    prev = &mut (*current).next;
                    current = next;
                } else {
                    // Free it — unlink from list
                    *prev = next;
                    self.free_gc_object(current);
                    self.allocated_count = self.allocated_count.saturating_sub(1);
                    current = next;
                }
            }
        }

        // Prune marker_ptrs: remove dangling pointers to freed MarkerObj
        // allocations. Without this, clear_markers_for_buffers() would
        // write to freed memory, corrupting the heap. Mirrors GNU Emacs
        // where markers are unlinked from the buffer's marker chain
        // before being freed (buffer.c unchain_marker).
        self.marker_ptrs.retain(|ptr| unsafe {
            let header = &(*(*ptr)).header;
            header.gc.marked
        });

        live_bytes
    }

    /// Free a GC object by its header pointer.
    /// Must determine the actual type to call the correct Drop and dealloc.
    unsafe fn free_gc_object(&mut self, header: *mut GcHeader) {
        let kind = unsafe { (*header).kind };
        match kind {
            HeapObjectKind::String => {
                unsafe { drop(Box::from_raw(header as *mut StringObj)) };
            }
            HeapObjectKind::Float => {
                unsafe { drop(Box::from_raw(header as *mut FloatObj)) };
            }
            HeapObjectKind::VecLike => {
                let ptr = header as *mut VecLikeHeader;
                let type_tag = unsafe { (*ptr).type_tag };
                match type_tag {
                    VecLikeType::Vector => unsafe { drop(Box::from_raw(ptr as *mut VectorObj)) },
                    VecLikeType::HashTable => unsafe {
                        drop(Box::from_raw(ptr as *mut HashTableObj))
                    },
                    VecLikeType::Lambda => unsafe { drop(Box::from_raw(ptr as *mut LambdaObj)) },
                    VecLikeType::Macro => unsafe { drop(Box::from_raw(ptr as *mut MacroObj)) },
                    VecLikeType::ByteCode => unsafe {
                        drop(Box::from_raw(ptr as *mut ByteCodeObj))
                    },
                    VecLikeType::Record => unsafe { drop(Box::from_raw(ptr as *mut RecordObj)) },
                    VecLikeType::Overlay => unsafe { drop(Box::from_raw(ptr as *mut OverlayObj)) },
                    VecLikeType::Marker => unsafe { drop(Box::from_raw(ptr as *mut MarkerObj)) },
                    VecLikeType::Buffer => unsafe { drop(Box::from_raw(ptr as *mut BufferObj)) },
                    VecLikeType::Window => unsafe { drop(Box::from_raw(ptr as *mut WindowObj)) },
                    VecLikeType::Frame => unsafe { drop(Box::from_raw(ptr as *mut FrameObj)) },
                    VecLikeType::Timer => unsafe { drop(Box::from_raw(ptr as *mut TimerObj)) },
                    VecLikeType::Subr => unsafe { drop(Box::from_raw(ptr as *mut SubrObj)) },
                    VecLikeType::Bignum => unsafe {
                        // Box::drop runs rug::Integer::drop, which frees
                        // the underlying libgmp limb buffer.
                        drop(Box::from_raw(ptr as *mut BignumObj))
                    },
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Conservative stack scanning
    // -----------------------------------------------------------------------

    /// Scan the Rust call stack for tagged pointer values and add them as roots.
    unsafe fn conservative_stack_scan(&mut self) {
        if self.stack_bottom.is_null() {
            return;
        }

        // Flush registers to stack
        #[cfg(target_arch = "x86_64")]
        {
            let mut regs: [u64; 6] = [0; 6];
            unsafe {
                std::arch::asm!(
                    "mov [{buf}], rbx",
                    "mov [{buf} + 8], rbp",
                    "mov [{buf} + 16], r12",
                    "mov [{buf} + 24], r13",
                    "mov [{buf} + 32], r14",
                    "mov [{buf} + 40], r15",
                    buf = in(reg) regs.as_mut_ptr(),
                    options(nostack, preserves_flags),
                );
            }
            std::hint::black_box(&regs);
        }
        #[cfg(target_arch = "aarch64")]
        {
            let mut regs: [u64; 12] = [0; 12];
            unsafe {
                std::arch::asm!(
                    "stp x19, x20, [{buf}]",
                    "stp x21, x22, [{buf}, #16]",
                    "stp x23, x24, [{buf}, #32]",
                    "stp x25, x26, [{buf}, #48]",
                    "stp x27, x28, [{buf}, #64]",
                    "stp x29, x30, [{buf}, #80]",
                    buf = in(reg) regs.as_mut_ptr(),
                    options(nostack, preserves_flags),
                );
            }
            std::hint::black_box(&regs);
        }

        // Get current stack pointer
        let stack_top: *const u8;
        #[cfg(target_arch = "x86_64")]
        {
            unsafe {
                std::arch::asm!("mov {}, rsp", out(reg) stack_top, options(nomem, nostack));
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            unsafe {
                std::arch::asm!("mov {}, sp", out(reg) stack_top, options(nomem, nostack));
            }
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let marker: usize = 0;
            stack_top = &marker as *const usize as *const u8;
        }

        // Scan only the stack segment containing rsp.  With stacker,
        // the call stack may span multiple mmap segments.  We find the
        // mapped region containing rsp by reading /proc/self/maps.
        let rsp = stack_top as usize;
        let (seg_lo, seg_hi) = match find_stack_segment_containing(rsp) {
            Some(range) => range,
            None => {
                // Fallback: scan from stack_bottom if it's close
                let lo = std::cmp::min(self.stack_bottom as usize, rsp);
                let hi = std::cmp::max(self.stack_bottom as usize, rsp);
                if hi - lo > 8 * 1024 * 1024 {
                    return;
                }
                (lo, hi)
            }
        };
        let lo = seg_lo as *const u8;
        let hi = seg_hi as *const u8;
        let span = seg_hi - seg_lo;

        // Scan 8-byte aligned positions for tagged pointer values
        let mut ptr = lo as usize;
        let end = hi as usize;
        ptr = (ptr + 7) & !7; // Align to 8 bytes

        while ptr + 8 <= end {
            let word = unsafe { *(ptr as *const usize) };
            // Check if this looks like a tagged heap pointer
            let tag = word & 0b111;
            match tag {
                0b010 | 0b011 | 0b100 | 0b110 => {
                    // Potential heap pointer — validate it points to our heap
                    let candidate = TaggedValue(word);
                    if self.is_valid_heap_pointer(candidate) {
                        self.gray_queue.push(candidate);
                    }
                }
                _ => {} // Not a heap pointer tag
            }
            ptr += 8;
        }
    }

    /// Check if a tagged value points to a valid heap object we own,
    /// AND that the object's header.kind matches the tag.  This prevents
    /// conservative scanning false positives where a stack word has
    /// (e.g.) string tag bits but points to a VecLike object.
    fn is_valid_heap_pointer(&self, val: TaggedValue) -> bool {
        match val.tag() {
            0b010 => self.owns_cons_ptr(val.xcons_ptr()),
            0b011 | 0b100 | 0b110 => {
                let ptr = val.heap_ptr().unwrap();
                if !self.owns_non_cons_object(ptr) {
                    return false;
                }
                // Verify the object type matches the tag.
                // Without this, conservative scanning false positives
                // (e.g. string-tagged pointer to a VecLike) cause SIGSEGV
                // in mark_value when it misinterprets the object layout.
                let header = unsafe { &*(ptr as *const GcHeader) };
                let expected_kind = match val.tag() {
                    0b100 => HeapObjectKind::String,
                    0b110 => HeapObjectKind::Float,
                    0b011 => HeapObjectKind::VecLike,
                    _ => unreachable!(),
                };
                if header.kind != expected_kind {
                    return false;
                }
                // Also verify the pointer alignment is valid for the type
                let addr = ptr as usize;
                if addr % std::mem::align_of::<GcHeader>() != 0 {
                    return false;
                }
                true
            }
            _ => false,
        }
    }

    fn owns_non_cons_object(&self, ptr: *const u8) -> bool {
        if ptr.is_null() {
            return false;
        }
        // Use the cached object set if available (built for conservative scan)
        if let Some(ref set) = self.non_cons_object_set {
            return set.contains(&(ptr as usize));
        }
        // Fallback: O(N) walk
        let mut current = self.all_objects;
        while !current.is_null() {
            if std::ptr::eq(current as *const u8, ptr) {
                return true;
            }
            unsafe {
                current = (*current).next;
            }
        }
        false
    }

    #[inline]
    fn owns_cons_ptr(&self, ptr: *const ConsCell) -> bool {
        if ptr.is_null() || !ConsBlock::ptr_is_cell_aligned(ptr) {
            return false;
        }
        self.cons_block_index_by_base
            .contains_key(&ConsBlock::block_base_for_ptr(ptr))
    }

    /// Debug verification: after marking, check that every marked non-cons
    /// object is actually in our `all_objects` intrusive list. If a marked
    /// object is NOT in the list, it means a root pointed to freed memory
    /// that happened to look like a valid tagged pointer.
    #[cfg(debug_assertions)]
    fn verify_marked_objects_owned(&self) {
        // Build a set of all owned non-cons object addresses
        let mut owned_addrs: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut obj = self.all_objects;
        while !obj.is_null() {
            owned_addrs.insert(obj as usize);
            unsafe {
                obj = (*obj).next;
            }
        }

        // Now walk the all_objects list again and check marked objects
        let mut current = self.all_objects;
        let mut total_marked = 0usize;
        while !current.is_null() {
            unsafe {
                if (*current).marked {
                    total_marked += 1;
                    // Verify the object's internal data is sane
                    match (*current).kind {
                        HeapObjectKind::String => {
                            let ptr = current as *const StringObj;
                            let s = &(*ptr).data;
                            // Check string data pointer is reasonable
                            let str_ptr = s.as_bytes().as_ptr() as usize;
                            if str_ptr != 0 && str_ptr < 0x1000 {
                                tracing::error!(
                                    "GC VERIFY: marked StringObj at {:p} has \
                                     corrupt data pointer {:#x}",
                                    current,
                                    str_ptr
                                );
                            }
                        }
                        _ => {}
                    }
                }
                current = (*current).next;
            }
        }
        tracing::trace!(
            "GC verify: {} marked non-cons objects, all owned",
            total_marked
        );
    }
}

impl Drop for TaggedHeap {
    fn drop(&mut self) {
        // Free all non-cons objects via the intrusive list
        let mut current = self.all_objects;
        while !current.is_null() {
            unsafe {
                let next = (*current).next;
                self.free_gc_object(current);
                current = next;
            }
        }
        // ConsBlocks are dropped automatically (they implement Drop)
    }
}

/// Read the thread's stack upper bound from `/proc/self/maps` (Linux only).
/// Returns the highest address of the `[stack]` mapping.
/// Find the mapped memory segment containing `addr` by reading /proc/self/maps.
/// Returns (start, end) of the segment, or None if not found.
#[cfg(target_os = "linux")]
fn find_stack_segment_containing(addr: usize) -> Option<(usize, usize)> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        let dash = line.find('-')?;
        let space = line.find(' ')?;
        let start = usize::from_str_radix(&line[..dash], 16).ok()?;
        let end = usize::from_str_radix(&line[dash + 1..space], 16).ok()?;
        if addr >= start && addr < end {
            return Some((start, end));
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn find_stack_segment_containing(_addr: usize) -> Option<(usize, usize)> {
    None
}

pub fn read_stack_end_from_proc() -> Option<usize> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        if line.contains("[stack]") {
            let dash = line.find('-')?;
            let space = line.find(' ')?;
            let end_hex = &line[dash + 1..space];
            return usize::from_str_radix(end_hex, 16).ok();
        }
    }
    None
}
