//! Mark-sweep garbage collector for the tagged pointer value system.
//!
//! # Design
//!
//! - **Cons cells**: block allocator with external mark bitmap.
//!   Each `ConsBlock` holds a fixed-size array of `ConsCell` plus a
//!   bitmap for marking and a free list.
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
use crate::gc::GcTrace;
use std::alloc::{self, Layout};
use std::cell::Cell;

// ---------------------------------------------------------------------------
// Thread-local heap access
// ---------------------------------------------------------------------------

thread_local! {
    static TAGGED_HEAP: Cell<*mut TaggedHeap> = const { Cell::new(std::ptr::null_mut()) };
    /// Auto-allocated heap for tests that construct Values without a Context.
    #[cfg(test)]
    static TEST_FALLBACK_TAGGED_HEAP: std::cell::RefCell<Option<Box<TaggedHeap>>> =
        const { std::cell::RefCell::new(None) };
}

/// Set the thread-local tagged heap pointer.
pub fn set_tagged_heap(heap: &mut TaggedHeap) {
    TAGGED_HEAP.with(|h| h.set(heap as *mut TaggedHeap));
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

// ---------------------------------------------------------------------------
// Cons block allocator
// ---------------------------------------------------------------------------

/// Number of cons cells per block. 4096 cells × 16 bytes = 64 KB per block.
const CONS_BLOCK_SIZE: usize = 4096;

/// A block of cons cells with an external mark bitmap.
struct ConsBlock {
    /// The cons cells. Allocated as a contiguous array.
    cells: *mut ConsCell,
    /// Mark bitmap: one bit per cell.
    marks: Vec<bool>,
    /// Free list: indices of unallocated cells within this block.
    free_list: Vec<u16>,
    /// How many cells are in use (allocated and not freed).
    used: usize,
}

impl ConsBlock {
    fn new() -> Self {
        let layout = Layout::array::<ConsCell>(CONS_BLOCK_SIZE).unwrap();
        let cells = unsafe { alloc::alloc_zeroed(layout) as *mut ConsCell };
        if cells.is_null() {
            alloc::handle_alloc_error(layout);
        }
        // Initialize free list (all cells available, in reverse order for LIFO)
        let free_list: Vec<u16> = (0..CONS_BLOCK_SIZE as u16).rev().collect();
        Self {
            cells,
            marks: vec![false; CONS_BLOCK_SIZE],
            free_list,
            used: 0,
        }
    }

    /// Allocate a cons cell from this block. Returns None if full.
    fn alloc(&mut self, car: TaggedValue, cdr: TaggedValue) -> Option<*mut ConsCell> {
        let idx = self.free_list.pop()?;
        let cell = unsafe { self.cells.add(idx as usize) };
        unsafe {
            (*cell).car = car;
            (*cell).cdr = cdr;
        }
        self.used += 1;
        Some(cell)
    }

    /// Check if a pointer falls within this block's cell array.
    fn contains(&self, ptr: *const ConsCell) -> bool {
        let base = self.cells as usize;
        let end = base + CONS_BLOCK_SIZE * std::mem::size_of::<ConsCell>();
        let p = ptr as usize;
        p >= base && p < end && (p - base) % std::mem::size_of::<ConsCell>() == 0
    }

    /// Get the index of a cell within this block.
    fn index_of(&self, ptr: *const ConsCell) -> usize {
        let offset = (ptr as usize) - (self.cells as usize);
        offset / std::mem::size_of::<ConsCell>()
    }

    /// Mark a cell by pointer.
    fn mark(&mut self, ptr: *const ConsCell) {
        let idx = self.index_of(ptr);
        self.marks[idx] = true;
    }

    /// Check if a cell is marked.
    fn is_marked(&self, ptr: *const ConsCell) -> bool {
        let idx = self.index_of(ptr);
        self.marks[idx]
    }

    /// Clear all marks.
    fn clear_marks(&mut self) {
        for m in &mut self.marks {
            *m = false;
        }
    }

    /// Sweep: free unmarked cells, return count of freed cells.
    fn sweep(&mut self) -> usize {
        let mut freed = 0;
        for i in 0..CONS_BLOCK_SIZE {
            if !self.marks[i] && !self.free_list.contains(&(i as u16)) {
                // Cell was in use but not marked — free it
                self.free_list.push(i as u16);
                self.used -= 1;
                freed += 1;
            }
        }
        freed
    }
}

impl Drop for ConsBlock {
    fn drop(&mut self) {
        let layout = Layout::array::<ConsCell>(CONS_BLOCK_SIZE).unwrap();
        unsafe { alloc::dealloc(self.cells as *mut u8, layout) };
    }
}

// ---------------------------------------------------------------------------
// TaggedHeap — the main GC-managed heap
// ---------------------------------------------------------------------------

/// The tagged pointer heap. Owns all heap-allocated Lisp objects.
pub struct TaggedHeap {
    /// Cons cell block allocator.
    cons_blocks: Vec<ConsBlock>,

    /// Intrusive linked list of all non-cons heap objects.
    /// Points to the GcHeader of the first object; follow `next` to traverse.
    all_objects: *mut GcHeader,

    /// Total number of allocated objects (cons + non-cons).
    pub allocated_count: usize,

    /// GC threshold: collect when allocated_count exceeds this.
    gc_threshold: usize,

    /// Gray worklist for mark phase.
    gray_queue: Vec<TaggedValue>,

    /// Stack bottom for conservative stack scanning.
    stack_bottom: *const u8,

    /// Tracking list of all allocated marker objects for bulk operations
    /// like clearing markers when buffers are killed.
    marker_ptrs: Vec<*mut MarkerObj>,
}

impl TaggedHeap {
    pub fn new() -> Self {
        Self {
            cons_blocks: Vec::new(),
            all_objects: std::ptr::null_mut(),
            allocated_count: 0,
            gc_threshold: 8192,
            gray_queue: Vec::new(),
            stack_bottom: std::ptr::null(),
            marker_ptrs: Vec::new(),
        }
    }

    pub fn set_stack_bottom(&mut self, bottom: *const u8) {
        self.stack_bottom = bottom;
    }

    pub fn should_collect(&self) -> bool {
        self.allocated_count >= self.gc_threshold
    }

    pub fn gc_threshold(&self) -> usize {
        self.gc_threshold
    }

    pub fn set_gc_threshold(&mut self, threshold: usize) {
        self.gc_threshold = threshold;
    }

    pub fn allocated_count(&self) -> usize {
        self.allocated_count
    }

    // -----------------------------------------------------------------------
    // Allocation
    // -----------------------------------------------------------------------

    /// Allocate a cons cell. Returns a tagged Value.
    pub fn alloc_cons(&mut self, car: TaggedValue, cdr: TaggedValue) -> TaggedValue {
        // Try existing blocks first
        for block in &mut self.cons_blocks {
            if let Some(cell) = block.alloc(car, cdr) {
                self.allocated_count += 1;
                return unsafe { TaggedValue::from_cons_ptr(cell) };
            }
        }
        // All blocks full — allocate a new block
        let mut block = ConsBlock::new();
        let cell = block
            .alloc(car, cdr)
            .expect("fresh block should have space");
        self.cons_blocks.push(block);
        self.allocated_count += 1;
        unsafe { TaggedValue::from_cons_ptr(cell) }
    }

    /// Allocate a string object.
    pub fn alloc_string(&mut self, s: crate::gc::types::LispString) -> TaggedValue {
        let obj = Box::new(StringObj {
            header: GcHeader::new(HeapObjectKind::String),
            data: s,
            text_props: TextPropertyTable::new(),
        });
        let ptr = Box::into_raw(obj);
        self.link_object(unsafe { &mut (*ptr).header });
        self.allocated_count += 1;
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
        unsafe { TaggedValue::from_float_ptr(ptr) }
    }

    /// Allocate a canonical subr object.
    pub fn alloc_subr(
        &mut self,
        name: crate::emacs_core::intern::SymId,
        function: Option<SubrFn>,
        min_args: u16,
        max_args: Option<u16>,
    ) -> TaggedValue {
        let obj = Box::new(SubrObj {
            header: VecLikeHeader::new(VecLikeType::Subr),
            name,
            min_args,
            max_args,
            function,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
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
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a lambda.
    /// Allocate a lambda (interpreted closure) as a Value vector.
    /// Matches GNU Emacs's PVEC_CLOSURE: all slots are GC-traced Values.
    pub fn alloc_lambda(&mut self, slots: Vec<TaggedValue>) -> TaggedValue {
        let obj = Box::new(LambdaObj {
            header: VecLikeHeader::new(VecLikeType::Lambda),
            data: slots,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
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
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
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
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate an overlay.
    pub fn alloc_overlay(&mut self, data: crate::gc::types::OverlayData) -> TaggedValue {
        let obj = Box::new(OverlayObj {
            header: VecLikeHeader::new(VecLikeType::Overlay),
            data,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    /// Allocate a marker.
    pub fn alloc_marker(&mut self, data: crate::gc::types::MarkerData) -> TaggedValue {
        let obj = Box::new(MarkerObj {
            header: VecLikeHeader::new(VecLikeType::Marker),
            data,
        });
        let ptr = Box::into_raw(obj);
        self.marker_ptrs.push(ptr);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }

    // -----------------------------------------------------------------------
    // Marker operations
    // -----------------------------------------------------------------------

    /// Clear buffer association for all markers belonging to any of the
    /// killed buffers.
    pub fn clear_markers_for_buffers(
        &mut self,
        killed: &std::collections::HashSet<crate::buffer::BufferId>,
    ) {
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

        // -- Conservative stack scan --
        unsafe { self.conservative_stack_scan() };

        // -- Mark phase: drain gray queue --
        self.mark_all();

        // -- Sweep phase --
        self.sweep_cons();
        self.sweep_objects();

        // -- Adapt threshold --
        self.gc_threshold = self.allocated_count.saturating_mul(2).max(8192);
    }

    /// Drain the gray queue, marking and tracing all reachable objects.
    fn mark_all(&mut self) {
        while let Some(val) = self.gray_queue.pop() {
            self.mark_value(val);
        }
    }

    /// Mark a single tagged value and push its children onto the gray queue.
    fn mark_value(&mut self, val: TaggedValue) {
        match val.tag() {
            0b010 => {
                // Cons
                let ptr = val.xcons_ptr();
                if self.mark_cons(ptr) {
                    // Newly marked — trace children
                    let car = unsafe { (*ptr).car };
                    let cdr = unsafe { (*ptr).cdr };
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
                    let mut roots = Vec::new();
                    (*ptr).text_props.trace_roots(&mut roots);
                    for root in roots {
                        if root.is_heap_object() {
                            self.gray_queue.push(root);
                        }
                    }
                };
            }
            0b110 => {
                // Float — no children
                let ptr = val.as_float_ptr().unwrap() as *mut FloatObj;
                unsafe { (*ptr).header.marked = true };
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
        for block in &mut self.cons_blocks {
            if block.contains(ptr) {
                if block.is_marked(ptr) {
                    return false; // Already marked
                }
                block.mark(ptr);
                return true;
            }
        }
        false // Not found in any block (shouldn't happen)
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
            | VecLikeType::Subr => {
                // These have no Value children to trace
            }
        }
    }

    /// Sweep unmarked cons cells back to free lists.
    fn sweep_cons(&mut self) {
        let mut total_freed = 0;
        for block in &mut self.cons_blocks {
            total_freed += block.sweep();
        }
        self.allocated_count = self.allocated_count.saturating_sub(total_freed);
    }

    /// Sweep non-cons objects: walk intrusive list, free unmarked, rebuild list.
    fn sweep_objects(&mut self) {
        let mut prev: *mut *mut GcHeader = &mut self.all_objects;
        let mut current = self.all_objects;
        while !current.is_null() {
            unsafe {
                let next = (*current).next;
                if (*current).marked {
                    // Keep it — advance prev
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
                    VecLikeType::HashTable => {
                        unsafe { drop(Box::from_raw(ptr as *mut HashTableObj)) }
                    }
                    VecLikeType::Lambda => unsafe { drop(Box::from_raw(ptr as *mut LambdaObj)) },
                    VecLikeType::Macro => unsafe { drop(Box::from_raw(ptr as *mut MacroObj)) },
                    VecLikeType::ByteCode => {
                        unsafe { drop(Box::from_raw(ptr as *mut ByteCodeObj)) }
                    }
                    VecLikeType::Record => unsafe { drop(Box::from_raw(ptr as *mut RecordObj)) },
                    VecLikeType::Overlay => {
                        unsafe { drop(Box::from_raw(ptr as *mut OverlayObj)) }
                    }
                    VecLikeType::Marker => unsafe { drop(Box::from_raw(ptr as *mut MarkerObj)) },
                    VecLikeType::Buffer => unsafe { drop(Box::from_raw(ptr as *mut BufferObj)) },
                    VecLikeType::Window => unsafe { drop(Box::from_raw(ptr as *mut WindowObj)) },
                    VecLikeType::Frame => unsafe { drop(Box::from_raw(ptr as *mut FrameObj)) },
                    VecLikeType::Timer => unsafe { drop(Box::from_raw(ptr as *mut TimerObj)) },
                    VecLikeType::Subr => unsafe { drop(Box::from_raw(ptr as *mut SubrObj)) },
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

        let (lo, hi) = if self.stack_bottom < stack_top {
            (self.stack_bottom, stack_top)
        } else {
            (stack_top, self.stack_bottom)
        };
        let span = (hi as usize).saturating_sub(lo as usize);
        if span == 0 || span > 64 * 1024 * 1024 {
            return;
        }

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

    /// Check if a tagged value points to a valid heap object we own.
    fn is_valid_heap_pointer(&self, val: TaggedValue) -> bool {
        match val.tag() {
            0b010 => {
                // Cons — check if pointer falls in any cons block
                let ptr = val.xcons_ptr();
                self.cons_blocks.iter().any(|b| b.contains(ptr))
            }
            0b011 | 0b100 | 0b110 => {
                let ptr = val.heap_ptr().unwrap();
                self.owns_non_cons_object(ptr)
            }
            _ => false,
        }
    }

    fn owns_non_cons_object(&self, ptr: *const u8) -> bool {
        if ptr.is_null() {
            return false;
        }
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
