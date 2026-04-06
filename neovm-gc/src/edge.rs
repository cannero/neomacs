use core::fmt;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::descriptor::{GcErased, Relocator, Trace, Tracer, trace_edge};
use crate::object::ObjectHeader;
use crate::root::Gc;

/// Interior-mutable managed edge slot.
pub struct EdgeCell<T: ?Sized> {
    value: AtomicPtr<ObjectHeader>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> EdgeCell<T> {
    /// Create a new edge slot with `value`.
    pub fn new(value: Option<Gc<T>>) -> Self {
        Self {
            value: AtomicPtr::new(match value {
                Some(value) => value.erase().as_raw(),
                None => core::ptr::null_mut(),
            }),
            _marker: PhantomData,
        }
    }

    /// Read the current edge value.
    pub fn get(&self) -> Option<Gc<T>> {
        let raw = self.value.load(Ordering::Acquire);
        unsafe { GcErased::from_raw(raw).map(|value| Gc::from_erased(value)) }
    }

    /// Replace the current edge value and return the previous one.
    pub fn replace(&self, value: Option<Gc<T>>) -> Option<Gc<T>> {
        let previous = self.value.swap(Self::raw_value(value), Ordering::AcqRel);
        unsafe { GcErased::from_raw(previous).map(|value| Gc::from_erased(value)) }
    }

    /// Overwrite the current edge value.
    pub fn set(&self, value: Option<Gc<T>>) {
        self.value.store(Self::raw_value(value), Ordering::Release);
    }

    fn raw_value(value: Option<Gc<T>>) -> *mut ObjectHeader {
        match value {
            Some(value) => value.erase().as_raw(),
            None => core::ptr::null_mut(),
        }
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

#[cfg(test)]
#[path = "edge_test.rs"]
mod tests;
