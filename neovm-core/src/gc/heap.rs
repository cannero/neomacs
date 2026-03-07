//! Arena-based heap with incremental tri-color mark-and-sweep collection.

use super::types::{HeapObject, ObjId};
use crate::emacs_core::bytecode::ByteCodeFunction;
use crate::emacs_core::value::{HashTableTest, LambdaData, LispHashTable, Value};

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
}

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
        }
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
        self.alloc(HeapObject::Str(crate::gc::types::LispString {
            text: s,
            multibyte,
        }))
    }

    pub fn alloc_string_with_flag(&mut self, s: String, multibyte: bool) -> ObjId {
        self.alloc(HeapObject::Str(crate::gc::types::LispString {
            text: s,
            multibyte,
        }))
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
    /// then drains the queue.
    pub fn rescan_roots(&mut self, roots: impl Iterator<Item = Value>) {
        debug_assert_eq!(self.gc_phase, GcPhase::Marking);
        for root in roots {
            Self::push_value_ids(&root, &mut self.gray_queue);
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
                    HeapObject::Str(s) => format!("Str({:?})", &s.text[..s.text.len().min(40)]),
                    HeapObject::Lambda(_) => "Lambda".to_string(),
                    HeapObject::Macro(_) => "Macro".to_string(),
                    HeapObject::ByteCode(_) => "ByteCode".to_string(),
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

    pub fn get_string(&self, id: ObjId) -> &String {
        match self.get(id) {
            HeapObject::Str(s) => &s.text,
            _ => panic!("get_string on non-string"),
        }
    }

    pub fn get_string_mut(&mut self, id: ObjId) -> &mut String {
        self.write_barrier(id);
        match self.get_mut(id) {
            HeapObject::Str(s) => &mut s.text,
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

    /// Collect garbage. `roots` must yield every Value that is reachable.
    ///
    /// Runs a complete mark-and-sweep cycle (mark all, then sweep all).
    /// Write barriers protect against mutations during future incremental
    /// collection where marking is interleaved with mutator execution.
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

            // Collect children into a local vec, then extend gray_queue.
            // This avoids borrow conflicts with self.objects / self.gray_queue.
            let mut children = Vec::new();
            Self::trace_heap_object(&self.objects[i], &mut children);
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
            | Value::ByteCode(id) => worklist.push(*id),
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
                for v in ht.data.values() {
                    Self::push_value_ids(v, children);
                }
                for v in ht.key_snapshots.values() {
                    Self::push_value_ids(v, children);
                }
            }
            HeapObject::Str(_) => {} // no Value children
            HeapObject::Lambda(d) | HeapObject::Macro(d) => {
                if let Some(env_val) = &d.env {
                    Self::push_value_ids(env_val, children);
                }
                // Trace OpaqueValues in body expressions — these hold
                // runtime Values (closures, byte-code, subrs) embedded in
                // the AST by value_to_expr / macro expansion.
                let mut opaque_values = Vec::new();
                for expr in d.body.iter() {
                    expr.collect_opaque_values(&mut opaque_values);
                }
                for v in &opaque_values {
                    Self::push_value_ids(v, children);
                }
            }
            HeapObject::ByteCode(bc) => {
                for c in &bc.constants {
                    Self::push_value_ids(c, children);
                }
                if let Some(env_val) = &bc.env {
                    Self::push_value_ids(env_val, children);
                }
            }
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

#[cfg(test)]
#[path = "heap_test.rs"]
mod tests;
