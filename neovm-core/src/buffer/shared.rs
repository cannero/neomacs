use std::cell::RefCell;
use std::rc::Rc;

use crate::emacs_core::value::Value;

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
