//! Arena-based heap with incremental tri-color mark-and-sweep collection.

use super::types::{HeapObject, MarkerData, ObjId, OverlayData};
use crate::buffer::BufferId;
use crate::emacs_core::bytecode::ByteCodeFunction;
use crate::emacs_core::value::{HashTableTest, LambdaData, LispHashTable, Value};
use std::collections::HashSet;

/// GC collection phase (tri-color incremental).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GcPhase {
    /// No collection in progress.
    Idle,
    /// Incremental mark phase — processing the gray worklist.
    Marking,
    /// Sweep phase — freeing unmarked objects.
    Sweeping,
}

/// The managed heap for cycle-forming Lisp objects.
pub struct LispHeap {
    objects: Vec<HeapObject>,
    generations: Vec<u32>,
    /// Tri-color mark bits: `true` = black (fully scanned), `false` = white.
    marks: Vec<bool>,
    free_list: Vec<u32>,
    allocated_count: usize,
    gc_threshold: usize,
    /// Current GC phase for incremental collection.
    gc_phase: GcPhase,
    /// Gray worklist — objects marked but whose children haven't been scanned.
    gray_queue: Vec<ObjId>,
    /// Stack bottom captured at creation — used by conservative stack scanning.
    /// Points to the deepest (oldest) stack frame of the thread that owns
    /// this heap.
    stack_bottom: *const u8,
}

// `stacker` can move deep recursion onto a separate stack segment. When that
// happens, the numeric address range between the captured thread stack-bottom
// and the current stack pointer is not a single readable mapping, so walking
// it conservatively can segfault. Keep the safety-net narrowly bounded and
// fall back to explicit roots outside that window.
const MAX_CONSERVATIVE_STACK_SCAN_BYTES: usize = 64 * 1024 * 1024;

impl LispHeap {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            generations: Vec::new(),
            marks: Vec::new(),
            free_list: Vec::new(),
            allocated_count: 0,
            gc_threshold: 8192,
            gc_phase: GcPhase::Idle,
            gray_queue: Vec::new(),
            // Will be set by Context::new() from the outermost frame
            stack_bottom: std::ptr::null(),
        }
    }

    /// Set the stack bottom pointer (call from the outermost frame).
    /// Must be called once from the thread's entry point or Context::new().
    pub fn set_stack_bottom(&mut self, bottom: *const u8) {
        self.stack_bottom = bottom;
    }

    // -----------------------------------------------------------------------
    // Allocation
    // -----------------------------------------------------------------------

    fn alloc(&mut self, obj: HeapObject) -> ObjId {
        self.allocated_count += 1;
        // During the marking phase, new allocations must be marked alive (black)
        // to prevent them from being swept. We also push their children to the
        // gray queue so existing objects they reference get scanned. Without this,
        // a new cons (car=X, cdr=Y) would survive but X/Y could be white and get
        // swept, causing stale ObjId panics.
        let mark_alive = self.gc_phase == GcPhase::Marking;

        // Trace children BEFORE moving obj into the heap (need the borrow).
        let mut children = Vec::new();
        if mark_alive {
            Self::trace_heap_object(&obj, &mut children);
        }

        let id = if let Some(idx) = self.free_list.pop() {
            let i = idx as usize;
            self.generations[i] = self.generations[i].wrapping_add(1);
            self.objects[i] = obj;
            self.marks[i] = mark_alive;
            ObjId {
                index: idx,
                generation: self.generations[i],
            }
        } else {
            let idx = self.objects.len() as u32;
            self.objects.push(obj);
            self.generations.push(0);
            self.marks.push(mark_alive);
            ObjId {
                index: idx,
                generation: 0,
            }
        };

        // Push children of new allocation to gray queue so they get scanned.
        if mark_alive && !children.is_empty() {
            self.gray_queue.extend(children);
        }

        id
    }

    pub fn alloc_cons(&mut self, car: Value, cdr: Value) -> ObjId {
        self.alloc(HeapObject::Cons { car, cdr })
    }

    pub fn alloc_vector(&mut self, items: Vec<Value>) -> ObjId {
        self.alloc(HeapObject::Vector(items))
    }

    pub fn alloc_hash_table(&mut self, test: HashTableTest) -> ObjId {
        self.alloc(HeapObject::HashTable(LispHashTable::new(test)))
    }

    pub fn alloc_hash_table_with_options(
        &mut self,
        test: HashTableTest,
        size: i64,
        weakness: Option<crate::emacs_core::value::HashTableWeakness>,
        rehash_size: f64,
        rehash_threshold: f64,
    ) -> ObjId {
        self.alloc(HeapObject::HashTable(LispHashTable::new_with_options(
            test,
            size,
            weakness,
            rehash_size,
            rehash_threshold,
        )))
    }

    pub fn alloc_hash_table_raw(&mut self, ht: LispHashTable) -> ObjId {
        self.alloc(HeapObject::HashTable(ht))
    }

    pub fn alloc_string(&mut self, s: String) -> ObjId {
        let multibyte = crate::encoding::is_multibyte_string(&s);
        self.alloc(HeapObject::Str(crate::gc::types::LispString::new(
            s, multibyte,
        )))
    }

    pub fn alloc_string_with_flag(&mut self, s: String, multibyte: bool) -> ObjId {
        self.alloc(HeapObject::Str(crate::gc::types::LispString::new(
            s, multibyte,
        )))
    }

    pub fn alloc_lisp_string(&mut self, s: crate::gc::types::LispString) -> ObjId {
        self.alloc(HeapObject::Str(s))
    }

    pub fn alloc_lambda(&mut self, data: LambdaData) -> ObjId {
        self.alloc(HeapObject::Lambda(data))
    }

    pub fn alloc_macro(&mut self, data: LambdaData) -> ObjId {
        self.alloc(HeapObject::Macro(data))
    }

    pub fn alloc_bytecode(&mut self, bc: ByteCodeFunction) -> ObjId {
        self.alloc(HeapObject::ByteCode(bc))
    }

    pub fn alloc_overlay(&mut self, overlay: OverlayData) -> ObjId {
        self.alloc(HeapObject::Overlay(overlay))
    }

    pub fn alloc_marker(&mut self, marker: MarkerData) -> ObjId {
        self.alloc(HeapObject::Marker(marker))
    }

    /// Current allocation threshold used by opportunistic GC call sites.
    pub fn gc_threshold(&self) -> usize {
        self.gc_threshold
    }

    /// Update the allocation threshold used by opportunistic GC call sites.
    /// Clamp to 1 so callers never disable threshold checks with zero.
    pub fn set_gc_threshold(&mut self, threshold: usize) {
        self.gc_threshold = threshold.max(1);
    }

    /// True when allocated objects reached the configured threshold.
    pub fn should_collect(&self) -> bool {
        self.allocated_count >= self.gc_threshold
    }

    /// True when an incremental marking cycle is in progress.
    pub fn is_marking(&self) -> bool {
        self.gc_phase == GcPhase::Marking
    }

    /// Begin an incremental marking cycle: clear marks, seed gray queue from
    /// roots, and set the phase to `Marking`.  Does NOT drain the queue.
    pub fn begin_marking(&mut self, roots: impl Iterator<Item = Value>) {
        self.gc_phase = GcPhase::Marking;
        for m in self.marks.iter_mut() {
            *m = false;
        }
        self.marks.resize(self.objects.len(), false);
        self.gray_queue.clear();
        for root in roots {
            Self::push_value_ids(&root, &mut self.gray_queue);
        }
    }

    /// Re-scan roots before sweeping.  Mutations to the root set (obarray,
    /// dynamic stack, temp_roots) during the incremental marking phase can
    /// introduce new live references that weren't in the initial root scan.
    /// This pushes any unmarked root values back to gray for re-tracing,
    /// does a conservative stack scan as safety net, then drains the queue.
    pub fn rescan_roots(&mut self, roots: impl Iterator<Item = Value>) {
        debug_assert_eq!(self.gc_phase, GcPhase::Marking);
        for root in roots {
            Self::push_value_ids(&root, &mut self.gray_queue);
        }
        // Conservative stack scan — catch any roots missed by explicit
        // enumeration, including Values in Rust local variables.
        // Flush registers to stack first (see collect() comment).
        unsafe {
            Self::flush_registers_and_scan_stack(self);
        }
        // Drain the gray queue (fast — only processes newly-discovered items).
        self.mark_all();
    }

    /// Finish an incremental collection cycle: sweep unmarked objects,
    /// adapt the threshold, and return to `Idle`.
    pub fn finish_collection(&mut self) {
        self.gc_phase = GcPhase::Sweeping;
        self.sweep_all();
        self.gc_phase = GcPhase::Idle;
        self.gc_threshold = self.allocated_count.saturating_mul(2).max(8192);
    }

    // -----------------------------------------------------------------------
    // Checked access
    // -----------------------------------------------------------------------

    /// Check if an ObjId is still valid (not stale).
    #[inline]
    pub fn is_valid(&self, id: ObjId) -> bool {
        let i = id.index as usize;
        i < self.objects.len() && self.generations[i] == id.generation
    }

    /// Extract ObjId from a Value, if it contains one.
    fn value_objid(val: &Value) -> Option<ObjId> {
        match val {
            Value::Cons(id) | Value::Vector(id) | Value::Record(id)
            | Value::HashTable(id) | Value::Str(id) | Value::Lambda(id)
            | Value::Macro(id) | Value::ByteCode(id) | Value::Overlay(id)
            | Value::Marker(id) => Some(*id),
            _ => None,
        }
    }

    #[inline]
    fn check(&self, id: ObjId) {
        let i = id.index as usize;
        if !(i < self.objects.len() && self.generations[i] == id.generation) {
            let cur_gen = if i < self.generations.len() {
                self.generations[i]
            } else {
                u32::MAX
            };
            // Log what was at the slot when it was last alive
            let cur_obj = if i < self.objects.len() {
                match &self.objects[i] {
                    HeapObject::Free => "Free".to_string(),
                    HeapObject::Cons { car, cdr } => format!("Cons(car={}, cdr={})", car, cdr),
                    HeapObject::Vector(v) => format!("Vector(len={})", v.len()),
                    HeapObject::HashTable(_) => "HashTable".to_string(),
                    HeapObject::Str(s) => {
                        let text = s.as_str();
                        format!("Str({:?})", &text[..text.len().min(40)])
                    }
                    HeapObject::Lambda(_) => "Lambda".to_string(),
                    HeapObject::Macro(_) => "Macro".to_string(),
                    HeapObject::ByteCode(_) => "ByteCode".to_string(),
                    HeapObject::Overlay(_) => "Overlay".to_string(),
                    HeapObject::Marker(_) => "Marker".to_string(),
                }
            } else {
                "out-of-bounds".to_string()
            };
            tracing::warn!("=== STALE OBJID DIAGNOSTIC ===");
            tracing::warn!("  ObjId: {:?}", id);
            tracing::warn!("  Current generation: {}", cur_gen);
            tracing::warn!(
                "  Generation delta: {}",
                cur_gen.wrapping_sub(id.generation)
            );
            tracing::warn!(
                "  Current object at slot: {}",
                &cur_obj[..cur_obj.len().min(200)]
            );
            tracing::warn!("  Heap size: {}", self.objects.len());
            tracing::warn!("  Allocated count: {}", self.allocated_count);
            tracing::warn!("  GC phase: {:?}", self.gc_phase);
            tracing::warn!("  Free list length: {}", self.free_list.len());
            tracing::warn!("==============================");
            panic!("stale ObjId: {:?} (current gen={})", id, cur_gen,);
        }
    }

    pub fn get(&self, id: ObjId) -> &HeapObject {
        self.check(id);
        &self.objects[id.index as usize]
    }

    pub fn get_mut(&mut self, id: ObjId) -> &mut HeapObject {
        self.check(id);
        &mut self.objects[id.index as usize]
    }

    // -----------------------------------------------------------------------
    // Write barrier
    // -----------------------------------------------------------------------

    /// Barrier-back write barrier: if `id` is black (marked) during the
    /// marking phase, push it back to gray so its new children get scanned.
    #[inline]
    fn write_barrier(&mut self, id: ObjId) {
        if self.gc_phase == GcPhase::Marking {
            let i = id.index as usize;
            if i < self.marks.len() && self.marks[i] {
                // Push back to gray — will be re-scanned.
                self.marks[i] = false;
                self.gray_queue.push(id);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Cons accessors
    // -----------------------------------------------------------------------

    pub fn cons_car(&self, id: ObjId) -> Value {
        match self.get(id) {
            HeapObject::Cons { car, .. } => *car,
            _ => panic!("cons_car on non-cons"),
        }
    }

    pub fn cons_cdr(&self, id: ObjId) -> Value {
        match self.get(id) {
            HeapObject::Cons { cdr, .. } => *cdr,
            _ => panic!("cons_cdr on non-cons"),
        }
    }

    pub fn set_car(&mut self, id: ObjId, val: Value) {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Cons { car, .. } => *car = val,
            _ => panic!("set_car on non-cons"),
        }
    }

    pub fn set_cdr(&mut self, id: ObjId, val: Value) {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Cons { cdr, .. } => *cdr = val,
            _ => panic!("set_cdr on non-cons"),
        }
    }

    // -----------------------------------------------------------------------
    // Vector accessors
    // -----------------------------------------------------------------------

    pub fn vector_ref(&self, id: ObjId, index: usize) -> Value {
        match self.get(id) {
            HeapObject::Vector(v) => v[index],
            _ => panic!("vector_ref on non-vector"),
        }
    }

    pub fn vector_set(&mut self, id: ObjId, index: usize, val: Value) {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Vector(v) => v[index] = val,
            _ => panic!("vector_set on non-vector"),
        }
    }

    pub fn vector_len(&self, id: ObjId) -> usize {
        match self.get(id) {
            HeapObject::Vector(v) => v.len(),
            _ => panic!("vector_len on non-vector"),
        }
    }

    pub fn get_vector(&self, id: ObjId) -> &Vec<Value> {
        match self.get(id) {
            HeapObject::Vector(v) => v,
            _ => panic!("get_vector on non-vector"),
        }
    }

    pub fn get_vector_mut(&mut self, id: ObjId) -> &mut Vec<Value> {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Vector(v) => v,
            _ => panic!("get_vector_mut on non-vector"),
        }
    }

    // -----------------------------------------------------------------------
    // HashTable accessors
    // -----------------------------------------------------------------------

    pub fn get_hash_table(&self, id: ObjId) -> &LispHashTable {
        match self.get(id) {
            HeapObject::HashTable(ht) => ht,
            _ => panic!("get_hash_table on non-hash-table"),
        }
    }

    pub fn get_hash_table_mut(&mut self, id: ObjId) -> &mut LispHashTable {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::HashTable(ht) => ht,
            _ => panic!("get_hash_table_mut on non-hash-table"),
        }
    }

    // -----------------------------------------------------------------------
    // String accessors
    // -----------------------------------------------------------------------

    pub fn get_string(&self, id: ObjId) -> &str {
        match self.get(id) {
            HeapObject::Str(s) => s.as_str(),
            _ => panic!("get_string on non-string"),
        }
    }

    pub fn get_lisp_string(&self, id: ObjId) -> &crate::gc::types::LispString {
        match self.get(id) {
            HeapObject::Str(s) => s,
            _ => panic!("get_lisp_string on non-string"),
        }
    }

    pub fn get_string_mut(&mut self, id: ObjId) -> &mut String {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Str(s) => s.make_mut(),
            _ => panic!("get_string_mut on non-string"),
        }
    }

    pub fn string_is_multibyte(&self, id: ObjId) -> bool {
        match self.get(id) {
            HeapObject::Str(s) => s.multibyte,
            _ => panic!("string_is_multibyte on non-string"),
        }
    }

    // -----------------------------------------------------------------------
    // Lambda / Macro accessors
    // -----------------------------------------------------------------------

    pub fn get_lambda(&self, id: ObjId) -> &LambdaData {
        match self.get(id) {
            HeapObject::Lambda(d) => d,
            _ => panic!("get_lambda on non-lambda"),
        }
    }

    pub fn get_lambda_mut(&mut self, id: ObjId) -> &mut LambdaData {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Lambda(d) => d,
            _ => panic!("get_lambda_mut on non-lambda"),
        }
    }

    pub fn get_macro_data(&self, id: ObjId) -> &LambdaData {
        match self.get(id) {
            HeapObject::Macro(d) => d,
            _ => panic!("get_macro_data on non-macro"),
        }
    }

    pub fn get_macro_data_mut(&mut self, id: ObjId) -> &mut LambdaData {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Macro(d) => d,
            _ => panic!("get_macro_data_mut on non-macro"),
        }
    }

    // -----------------------------------------------------------------------
    // ByteCode accessors
    // -----------------------------------------------------------------------

    pub fn get_bytecode(&self, id: ObjId) -> &ByteCodeFunction {
        match self.get(id) {
            HeapObject::ByteCode(bc) => bc,
            _ => panic!("get_bytecode on non-bytecode"),
        }
    }

    pub fn get_bytecode_mut(&mut self, id: ObjId) -> &mut ByteCodeFunction {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::ByteCode(bc) => bc,
            _ => panic!("get_bytecode_mut on non-bytecode"),
        }
    }

    pub fn get_overlay(&self, id: ObjId) -> &OverlayData {
        match self.get(id) {
            HeapObject::Overlay(overlay) => overlay,
            _ => panic!("get_overlay on non-overlay"),
        }
    }

    pub fn get_overlay_mut(&mut self, id: ObjId) -> &mut OverlayData {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Overlay(overlay) => overlay,
            _ => panic!("get_overlay_mut on non-overlay"),
        }
    }

    pub fn get_marker(&self, id: ObjId) -> &MarkerData {
        match self.get(id) {
            HeapObject::Marker(marker) => marker,
            _ => panic!("get_marker on non-marker"),
        }
    }

    pub fn get_marker_mut(&mut self, id: ObjId) -> &mut MarkerData {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Marker(marker) => marker,
            _ => panic!("get_marker_mut on non-marker"),
        }
    }

    pub fn clear_markers_for_buffers(&mut self, killed: &HashSet<BufferId>) {
        for object in &mut self.objects {
            if let HeapObject::Marker(marker) = object
                && marker.buffer.is_some_and(|buffer| killed.contains(&buffer))
            {
                marker.buffer = None;
                marker.position = None;
            }
        }
    }

    // -----------------------------------------------------------------------
    // List helpers
    // -----------------------------------------------------------------------

    pub fn list_to_vec(&self, value: &Value) -> Option<Vec<Value>> {
        let mut result = Vec::new();
        let mut cursor = *value;
        loop {
            match cursor {
                Value::Nil => return Some(result),
                Value::Cons(id) => {
                    result.push(self.cons_car(id));
                    cursor = self.cons_cdr(id);
                }
                _ => return None,
            }
        }
    }

    pub fn list_length(&self, value: &Value) -> Option<usize> {
        let mut len = 0;
        let mut cursor = *value;
        loop {
            match cursor {
                Value::Nil => return Some(len),
                Value::Cons(id) => {
                    len += 1;
                    cursor = self.cons_cdr(id);
                }
                _ => return None,
            }
        }
    }

    // -----------------------------------------------------------------------
    // Structural equality
    // -----------------------------------------------------------------------

    pub fn equal_value(&self, a: &Value, b: &Value, depth: usize) -> bool {
        if depth > 4096 {
            return false;
        }
        match (a, b) {
            (Value::Cons(ai), Value::Cons(bi)) => {
                if ai == bi {
                    return true;
                }
                let a_car = self.cons_car(*ai);
                let a_cdr = self.cons_cdr(*ai);
                let b_car = self.cons_car(*bi);
                let b_cdr = self.cons_cdr(*bi);
                self.equal_value(&a_car, &b_car, depth + 1)
                    && self.equal_value(&a_cdr, &b_cdr, depth + 1)
            }
            (Value::Vector(ai), Value::Vector(bi)) | (Value::Record(ai), Value::Record(bi)) => {
                if ai == bi {
                    return true;
                }
                let av = self.get_vector(*ai);
                let bv = self.get_vector(*bi);
                av.len() == bv.len()
                    && av
                        .iter()
                        .zip(bv.iter())
                        .all(|(x, y)| self.equal_value(x, y, depth + 1))
            }
            _ => crate::emacs_core::value::equal_value(a, b, depth),
        }
    }

    // -----------------------------------------------------------------------
    // Incremental tri-color mark-and-sweep collection
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Conservative stack scanning
    // -----------------------------------------------------------------------

    /// Conservatively scan a memory region for potential ObjId references.
    ///
    /// Walks `[bottom, top)` word-by-word (4-byte aligned for u32 pairs).
    /// Any (index, generation) pair that matches a live heap slot is treated
    /// as a root. False positives are safe (just keep extra objects alive).
    ///
    /// This is the same strategy used by GNU Emacs (`mark_memory` in alloc.c),
    /// Flush all callee-saved CPU registers into a stack-local buffer,
    /// then run the conservative stack scan. This ensures Values held in
    /// registers (common in release builds) are visible to the scanner.
    ///
    /// # Safety
    /// Must be called from a context where `self` is valid and the stack
    /// is well-formed.
    #[inline(never)] // Must not be inlined — we need its stack frame to hold the spilled regs
    unsafe fn flush_registers_and_scan_stack(heap: *mut Self) {
        // Spill callee-saved registers to the stack so the scanner can see them.
        #[cfg(target_arch = "x86_64")]
        {
            // x86_64 callee-saved: rbx, rbp, r12, r13, r14, r15
            // We read them into a local array which lives on the stack.
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
            // Prevent the compiler from optimizing away the register spill
            std::hint::black_box(&regs);
        }
        #[cfg(target_arch = "aarch64")]
        {
            // aarch64 callee-saved: x19-x28, x29(fp), x30(lr)
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

        unsafe {
            let heap = &mut *heap;
            if let Some((lo, hi)) = heap.conservative_stack_scan_bounds() {
                heap.scan_stack_conservative(lo, hi);
            }
        }
    }

    /// Ruby, and Lua to catch roots that explicit enumeration might miss
    /// (e.g., Values in Rust local variables or registers).
    ///
    /// # Safety
    /// `bottom` and `top` must be valid readable memory. Typically these
    /// are the current thread's stack bounds.
    pub unsafe fn scan_stack_conservative(&mut self, bottom: *const u8, top: *const u8) {
        if bottom.is_null() || top.is_null() || bottom >= top {
            return;
        }
        let heap_len = self.objects.len() as u32;
        if heap_len == 0 {
            return;
        }

        // Scan 4-byte aligned positions for (index, generation) pairs.
        // ObjId is (u32 index, u32 generation) = 8 bytes.
        let mut ptr = bottom as usize;
        let end = top as usize;
        // Align to 4-byte boundary
        ptr = (ptr + 3) & !3;

        while ptr + 8 <= end {
            let index = unsafe { *(ptr as *const u32) };
            let generation = unsafe { *((ptr + 4) as *const u32) };

            // Check if this looks like a valid, live ObjId
            if index < heap_len {
                let i = index as usize;
                if i < self.generations.len()
                    && self.generations[i] == generation
                    && !matches!(self.objects[i], HeapObject::Free)
                    && !self.marks.get(i).copied().unwrap_or(true)
                {
                    // Found a potential live reference — mark it
                    let id = ObjId { index, generation };
                    self.gray_queue.push(id);
                }
            }

            ptr += 4; // Step by 4 bytes (not 8) to catch unaligned pairs
        }
    }

    /// Get an approximate stack pointer for conservative scanning.
    /// The returned pointer is on the current stack frame.
    ///
    /// # Safety
    /// The returned pointer is only valid for the duration of the calling
    /// function's stack frame.
    #[inline(always)]
    unsafe fn current_stack_ptr() -> *const u8 {
        let mut sp: *const u8;
        #[cfg(target_arch = "x86_64")]
        {
            unsafe {
                std::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack));
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            unsafe {
                std::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack));
            }
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            // Fallback: use a local variable's address as approximate SP
            let marker: usize = 0;
            sp = &marker as *const usize as *const u8;
        }
        sp
    }

    unsafe fn conservative_stack_scan_bounds(&self) -> Option<(*const u8, *const u8)> {
        if self.stack_bottom.is_null() {
            tracing::warn!("stack_bottom is NULL — conservative scan disabled");
            return None;
        }
        let stack_top = unsafe { Self::current_stack_ptr() };
        let (lo, hi) = if self.stack_bottom < stack_top {
            (self.stack_bottom, stack_top)
        } else {
            (stack_top, self.stack_bottom)
        };
        let span = (hi as usize).saturating_sub(lo as usize);
        if span == 0 || span > MAX_CONSERVATIVE_STACK_SCAN_BYTES {
            tracing::warn!(span, max = MAX_CONSERVATIVE_STACK_SCAN_BYTES, "stack scan skipped: span out of range");
            return None;
        }
        Some((lo, hi))
    }

    /// Collect garbage. `roots` must yield every Value that is reachable.
    ///
    /// Runs a complete mark-and-sweep cycle (mark all, then sweep all).
    /// Conservative stack scanning provides a safety net for any roots
    /// missed by explicit enumeration.
    #[tracing::instrument(level = "debug", skip(self, roots), fields(objects = self.objects.len(), allocated = self.allocated_count))]
    pub fn collect(&mut self, roots: impl Iterator<Item = Value>) {
        // -- Begin mark phase --
        self.gc_phase = GcPhase::Marking;

        // Clear marks
        for m in self.marks.iter_mut() {
            *m = false;
        }
        self.marks.resize(self.objects.len(), false);
        self.gray_queue.clear();

        // Seed gray queue from roots
        for root in roots {
            Self::push_value_ids(&root, &mut self.gray_queue);
        }

        // Drain gray queue (full mark)
        self.mark_all();

        // Conservative stack scan — safety net for any roots missed by
        // explicit enumeration. Scans the current thread's stack for
        // anything that looks like a valid ObjId and marks it.
        //
        // IMPORTANT: We must flush CPU registers to the stack first.
        // Release-mode optimizations keep Values in registers which are
        // invisible to the memory scanner. We spill all callee-saved
        // registers into a local array, which the stack scanner then
        // covers. This is the same principle used by GNU Emacs (setjmp)
        // and Boehm GC (getcontext).
        let gray_before = self.gray_queue.len();
        unsafe {
            Self::flush_registers_and_scan_stack(self);
        }
        let stack_found = self.gray_queue.len() - gray_before;
        if stack_found > 0 {
            tracing::debug!(stack_found, "conservative stack scan found roots");
        }
        // Mark any additional objects found by stack scan
        self.mark_all();

        // -- Sweep phase --
        self.gc_phase = GcPhase::Sweeping;
        self.sweep_all();

        // -- Done --
        self.gc_phase = GcPhase::Idle;

        // Adapt threshold: next GC triggers at 2x surviving objects, minimum 8192
        self.gc_threshold = self.allocated_count.saturating_mul(2).max(8192);
    }

    /// Process gray objects until the worklist is empty.
    fn mark_all(&mut self) {
        while let Some(id) = self.gray_queue.pop() {
            let i = id.index as usize;
            if i >= self.marks.len() || self.marks[i] {
                continue;
            }
            if self.generations[i] != id.generation {
                continue; // stale
            }
            self.marks[i] = true;

            let mut children = Vec::new();
            Self::trace_heap_object(&self.objects[i], &mut children);

            // NOTE: OpaqueValue roots from Lambda/Macro body ASTs are now
            // traced via the thread-local OpaqueValuePool in collect_roots,
            // not by walking body Expr trees here.

            self.gray_queue.extend(children);
        }
    }

    /// Process up to `work_limit` gray objects. Returns `true` when the gray
    /// queue is empty (marking complete).
    ///
    /// This enables future incremental collection: call `mark_some()` at
    /// safe points to spread marking work across multiple mutator pauses.
    pub fn mark_some(&mut self, work_limit: usize) -> bool {
        for _ in 0..work_limit {
            let Some(id) = self.gray_queue.pop() else {
                return true; // done
            };
            let i = id.index as usize;
            if i >= self.marks.len() || self.marks[i] {
                continue;
            }
            if self.generations[i] != id.generation {
                continue; // stale
            }
            self.marks[i] = true;

            let mut children = Vec::new();
            Self::trace_heap_object(&self.objects[i], &mut children);

            // NOTE: OpaqueValue roots traced via OpaqueValuePool (see mark_all).

            self.gray_queue.extend(children);
        }
        self.gray_queue.is_empty()
    }

    /// Sweep all unmarked objects in one pass.
    fn sweep_all(&mut self) {
        for i in 0..self.objects.len() {
            if !self.marks[i] && !matches!(self.objects[i], HeapObject::Free) {
                self.objects[i] = HeapObject::Free;
                self.generations[i] = self.generations[i].wrapping_add(1);
                self.free_list.push(i as u32);
                self.allocated_count = self.allocated_count.saturating_sub(1);
            }
        }
    }

    fn push_value_ids(val: &Value, worklist: &mut Vec<ObjId>) {
        match val {
            Value::Cons(id)
            | Value::Vector(id)
            | Value::Record(id)
            | Value::HashTable(id)
            | Value::Str(id)
            | Value::Lambda(id)
            | Value::Macro(id)
            | Value::ByteCode(id)
            | Value::Overlay(id)
            | Value::Marker(id) => worklist.push(*id),
            _ => {}
        }
    }

    /// Extract ObjIds from a HashKey and push them onto the worklist.
    /// HashKey can contain ObjIds in Str, ObjId, EqualCons, and EqualVec variants.
    fn push_hash_key_ids(key: &crate::emacs_core::value::HashKey, worklist: &mut Vec<ObjId>) {
        use crate::emacs_core::value::HashKey;
        match key {
            HashKey::Str(id) => worklist.push(*id),
            HashKey::ObjId(index, generation) => {
                worklist.push(ObjId {
                    index: *index,
                    generation: *generation,
                });
            }
            HashKey::EqualCons(car, cdr) => {
                Self::push_hash_key_ids(car, worklist);
                Self::push_hash_key_ids(cdr, worklist);
            }
            HashKey::EqualVec(items) => {
                for item in items.iter() {
                    Self::push_hash_key_ids(item, worklist);
                }
            }
            // Nil, True, Int, Float, FloatEq, Symbol, Keyword, Char,
            // Window, Frame, Ptr — no heap references
            _ => {}
        }
    }

    /// Trace all Value children inside a HeapObject, pushing their ObjIds onto
    /// the worklist.  Used by both `mark_all()` and `mark_some()`.
    fn trace_heap_object(obj: &HeapObject, children: &mut Vec<ObjId>) {
        match obj {
            HeapObject::Cons { car, cdr } => {
                Self::push_value_ids(car, children);
                Self::push_value_ids(cdr, children);
            }
            HeapObject::Vector(v) => {
                for val in v {
                    Self::push_value_ids(val, children);
                }
            }
            HeapObject::HashTable(ht) => {
                // Trace both keys AND values — HashKey can contain ObjIds
                // (e.g. HashKey::Str(ObjId), HashKey::ObjId(index, gen))
                for (k, v) in &ht.data {
                    Self::push_hash_key_ids(k, children);
                    Self::push_value_ids(v, children);
                }
                for (k, v) in &ht.key_snapshots {
                    Self::push_hash_key_ids(k, children);
                    Self::push_value_ids(v, children);
                }
            }
            HeapObject::Str(_) => {} // no Value children
            HeapObject::Lambda(d) | HeapObject::Macro(d) => {
                if let Some(env_val) = &d.env {
                    Self::push_value_ids(env_val, children);
                }
                if let Some(doc_val) = &d.doc_form {
                    Self::push_value_ids(doc_val, children);
                }
                if let Some(interactive_val) = &d.interactive {
                    Self::push_value_ids(interactive_val, children);
                }
                // NOTE: OpaqueValueRef indices in body ASTs are traced via
                // the thread-local OpaqueValuePool in collect_roots, not here.
            }
            HeapObject::ByteCode(bc) => {
                for c in &bc.constants {
                    Self::push_value_ids(c, children);
                }
                if let Some(env_val) = &bc.env {
                    Self::push_value_ids(env_val, children);
                }
                if let Some(doc_val) = &bc.doc_form {
                    Self::push_value_ids(doc_val, children);
                }
                if let Some(interactive_val) = &bc.interactive {
                    Self::push_value_ids(interactive_val, children);
                }
            }
            HeapObject::Overlay(overlay) => {
                Self::push_value_ids(&overlay.plist, children);
            }
            HeapObject::Marker(_) => {}
            HeapObject::Free => {}
        }
    }

    // -----------------------------------------------------------------------
    // Stats
    // -----------------------------------------------------------------------

    pub fn allocated_count(&self) -> usize {
        self.allocated_count
    }

    // -----------------------------------------------------------------------
    // pdump accessors
    // -----------------------------------------------------------------------

    /// Access all heap objects (for pdump serialization).
    pub(crate) fn objects(&self) -> &[HeapObject] {
        &self.objects
    }

    /// Mutable access to objects (for pdump hash table phase 2).
    pub(crate) fn objects_mut(&mut self) -> &mut [HeapObject] {
        &mut self.objects
    }

    /// Access generation counters (for pdump serialization).
    pub(crate) fn generations(&self) -> &[u32] {
        &self.generations
    }

    /// Access the free list (for pdump serialization).
    pub(crate) fn free_list(&self) -> &[u32] {
        &self.free_list
    }

    /// Reconstruct a LispHeap from pdump data.
    pub(crate) fn from_dump(
        objects: Vec<HeapObject>,
        generations: Vec<u32>,
        free_list: Vec<u32>,
    ) -> Self {
        let allocated_count = objects
            .iter()
            .filter(|o| !matches!(o, HeapObject::Free))
            .count();
        let marks = vec![false; objects.len()];
        Self {
            objects,
            generations,
            marks,
            free_list,
            allocated_count,
            gc_threshold: 8192,
            gc_phase: GcPhase::Idle,
            gray_queue: Vec::new(),
            stack_bottom: std::ptr::null(),
        }
    }
}

impl Default for LispHeap {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Read the stack end address from /proc/self/maps on Linux.
/// Returns the upper bound of the `[stack]` mapping.
#[cfg(target_os = "linux")]
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

#[cfg(test)]
#[path = "heap_test.rs"]
mod tests;
