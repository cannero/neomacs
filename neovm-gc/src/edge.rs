use core::cell::Cell;
use core::fmt;

use crate::descriptor::{Relocator, Trace, Tracer, trace_edge};
use crate::root::Gc;

/// Interior-mutable managed edge slot.
pub struct EdgeCell<T: ?Sized> {
    value: Cell<Option<Gc<T>>>,
}

impl<T: ?Sized> EdgeCell<T> {
    /// Create a new edge slot with `value`.
    pub const fn new(value: Option<Gc<T>>) -> Self {
        Self {
            value: Cell::new(value),
        }
    }

    /// Read the current edge value.
    pub fn get(&self) -> Option<Gc<T>> {
        self.value.get()
    }

    /// Replace the current edge value and return the previous one.
    pub fn replace(&self, value: Option<Gc<T>>) -> Option<Gc<T>> {
        self.value.replace(value)
    }

    /// Overwrite the current edge value.
    pub fn set(&self, value: Option<Gc<T>>) {
        self.value.set(value);
    }
}

impl<T: ?Sized> Default for EdgeCell<T> {
    fn default() -> Self {
        Self::new(None)
    }
}

impl<T: ?Sized> fmt::Debug for EdgeCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EdgeCell")
            .field("is_some", &self.get().is_some())
            .finish()
    }
}

unsafe impl<T: ?Sized> Trace for EdgeCell<T> {
    fn trace(&self, tracer: &mut dyn Tracer) {
        if let Some(value) = self.get() {
            trace_edge(tracer, value);
        }
    }

    fn relocate(&self, relocator: &mut dyn Relocator) {
        if let Some(value) = self.get() {
            let relocated = relocator.relocate_erased(value.erase());
            self.set(Some(unsafe { Gc::from_erased(relocated) }));
        }
    }
}
