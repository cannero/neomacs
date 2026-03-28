use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::emacs_core::value::Value;
use crate::gc::GcTrace;

use super::text_props::{PropertyInterval, TextPropertyTable};

#[derive(Clone)]
pub struct BufferTextProperties {
    inner: Rc<RefCell<TextPropertyTable>>,
}

impl Default for BufferTextProperties {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferTextProperties {
    pub fn new() -> Self {
        Self::from_table(TextPropertyTable::new())
    }

    pub fn from_table(table: TextPropertyTable) -> Self {
        Self {
            inner: Rc::new(RefCell::new(table)),
        }
    }

    pub fn shares_with(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn snapshot(&self) -> TextPropertyTable {
        self.inner.borrow().clone()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.borrow().is_empty()
    }

    pub fn put_property(&self, start: usize, end: usize, name: &str, value: Value) -> bool {
        self.inner
            .borrow_mut()
            .put_property(start, end, name, value)
    }

    pub fn get_property(&self, pos: usize, name: &str) -> Option<Value> {
        self.inner.borrow().get_property(pos, name).copied()
    }

    pub fn get_properties(&self, pos: usize) -> HashMap<String, Value> {
        self.inner.borrow().get_properties(pos)
    }

    pub fn get_properties_ordered(&self, pos: usize) -> Vec<(String, Value)> {
        self.inner.borrow().get_properties_ordered(pos)
    }

    pub fn remove_property(&self, start: usize, end: usize, name: &str) -> bool {
        self.inner.borrow_mut().remove_property(start, end, name)
    }

    pub fn remove_all_properties(&self, start: usize, end: usize) {
        self.inner.borrow_mut().remove_all_properties(start, end);
    }

    pub fn next_property_change(&self, pos: usize) -> Option<usize> {
        self.inner.borrow().next_property_change(pos)
    }

    pub fn previous_property_change(&self, pos: usize) -> Option<usize> {
        self.inner.borrow().previous_property_change(pos)
    }

    pub fn append_shifted(&self, other: &TextPropertyTable, byte_offset: usize) {
        self.inner.borrow_mut().append_shifted(other, byte_offset);
    }

    pub fn slice(&self, start: usize, end: usize) -> TextPropertyTable {
        self.inner.borrow().slice(start, end)
    }

    pub fn intervals_snapshot(&self) -> Vec<PropertyInterval> {
        self.inner.borrow().intervals_snapshot()
    }

    pub fn adjust_for_insert(&self, pos: usize, len: usize) {
        self.inner.borrow_mut().adjust_for_insert(pos, len);
    }

    pub fn adjust_for_delete(&self, start: usize, end: usize) {
        self.inner.borrow_mut().adjust_for_delete(start, end);
    }

    pub fn trace_roots(&self, roots: &mut Vec<Value>) {
        self.inner.borrow().trace_roots(roots);
    }
}

#[derive(Clone)]
pub struct SharedUndoState {
    inner: Rc<RefCell<SharedUndoStateInner>>,
}

#[derive(Clone)]
struct SharedUndoStateInner {
    list: Value,
    in_progress: bool,
    recorded_first_change: bool,
}

impl SharedUndoState {
    pub fn new() -> Self {
        Self::from_parts(Value::Nil, false, false)
    }

    pub fn from_parts(list: Value, in_progress: bool, recorded_first_change: bool) -> Self {
        Self {
            inner: Rc::new(RefCell::new(SharedUndoStateInner {
                list,
                in_progress,
                recorded_first_change,
            })),
        }
    }

    pub fn shares_with(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn list(&self) -> Value {
        self.inner.borrow().list
    }

    pub fn set_list(&self, list: Value) {
        self.inner.borrow_mut().list = list;
    }

    pub fn in_progress(&self) -> bool {
        self.inner.borrow().in_progress
    }

    pub fn set_in_progress(&self, in_progress: bool) {
        self.inner.borrow_mut().in_progress = in_progress;
    }

    pub fn recorded_first_change(&self) -> bool {
        self.inner.borrow().recorded_first_change
    }

    pub fn set_recorded_first_change(&self, recorded_first_change: bool) {
        self.inner.borrow_mut().recorded_first_change = recorded_first_change;
    }

    pub fn trace_roots(&self, roots: &mut Vec<Value>) {
        roots.push(self.list());
    }
}
