use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::descriptor::{EphemeronVisitor, GcErased, WeakProcessor};
use crate::object::ObjectHeader;
use crate::root::Gc;

/// Weak reference to a managed object.
#[derive(Debug)]
pub struct Weak<T: ?Sized> {
    target: Option<Gc<T>>,
}

impl<T: ?Sized> Weak<T> {
    /// Create a weak handle from an existing managed object.
    pub const fn new(target: Gc<T>) -> Self {
        Self {
            target: Some(target),
        }
    }

    /// Create an empty weak reference.
    pub const fn empty() -> Self {
        Self { target: None }
    }

    /// Return the underlying weak target when still known.
    pub fn target(&self) -> Option<Gc<T>> {
        self.target
    }
}

impl<T: ?Sized> Copy for Weak<T> {}

impl<T: ?Sized> Clone for Weak<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> PartialEq for Weak<T> {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
    }
}

impl<T: ?Sized> Eq for Weak<T> {}

impl<T: ?Sized> Hash for Weak<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.target.hash(state);
    }
}

/// Interior-mutable weak edge slot.
pub struct WeakCell<T: ?Sized> {
    value: AtomicPtr<ObjectHeader>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> WeakCell<T> {
    /// Create a weak slot with `value`.
    pub fn new(value: Weak<T>) -> Self {
        Self {
            value: AtomicPtr::new(Self::raw_value(value)),
            _marker: PhantomData,
        }
    }

    /// Read the current weak value.
    pub fn get(&self) -> Weak<T> {
        match self.load_target() {
            Some(target) => Weak::new(target),
            None => Weak::empty(),
        }
    }

    /// Return the current weak target when still known.
    pub fn target(&self) -> Option<Gc<T>> {
        self.load_target()
    }

    /// Overwrite the current weak value.
    pub fn set(&self, value: Weak<T>) {
        self.value.store(Self::raw_value(value), Ordering::Release);
    }

    /// Clear the current weak target.
    pub fn clear(&self) {
        self.set(Weak::empty());
    }

    /// Process this weak slot against the current collector liveness view.
    pub fn process(&self, processor: &mut dyn WeakProcessor) {
        if let Some(target) = self.target() {
            let remapped = processor.remap_or_drop(target.erase());
            if let Some(object) = remapped {
                self.set(Weak::new(unsafe { Gc::from_erased(object) }));
            } else {
                self.clear();
            }
        }
    }

    fn raw_value(value: Weak<T>) -> *mut ObjectHeader {
        match value.target() {
            Some(target) => target.erase().as_raw(),
            None => core::ptr::null_mut(),
        }
    }

    fn load_target(&self) -> Option<Gc<T>> {
        let raw = self.value.load(Ordering::Acquire);
        unsafe { GcErased::from_raw(raw).map(|value| Gc::from_erased(value)) }
    }
}

impl<T: ?Sized> Default for WeakCell<T> {
    fn default() -> Self {
        Self::new(Weak::empty())
    }
}

impl<T: ?Sized> fmt::Debug for WeakCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WeakCell")
            .field("is_some", &self.target().is_some())
            .finish()
    }
}

/// Token identifying one weak-map instance in the collector.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct WeakMapToken(pub u64);

/// Interior-mutable ephemeron slot.
pub struct Ephemeron<K: ?Sized, V: ?Sized> {
    key: AtomicPtr<ObjectHeader>,
    value: AtomicPtr<ObjectHeader>,
    _key_marker: PhantomData<fn() -> K>,
    _value_marker: PhantomData<fn() -> V>,
}

impl<K: ?Sized, V: ?Sized> Ephemeron<K, V> {
    /// Create a new ephemeron entry.
    pub fn new(key: Weak<K>, value: Weak<V>) -> Self {
        Self {
            key: AtomicPtr::new(Self::raw_value(key)),
            value: AtomicPtr::new(Self::raw_value(value)),
            _key_marker: PhantomData,
            _value_marker: PhantomData,
        }
    }

    /// Create an empty ephemeron entry.
    pub fn empty() -> Self {
        Self::new(Weak::empty(), Weak::empty())
    }

    /// Return the current ephemeron key when still known.
    pub fn key(&self) -> Option<Gc<K>> {
        let raw = self.key.load(Ordering::Acquire);
        unsafe { GcErased::from_raw(raw).map(|value| Gc::from_erased(value)) }
    }

    /// Return the current ephemeron value when still known.
    pub fn value(&self) -> Option<Gc<V>> {
        let raw = self.value.load(Ordering::Acquire);
        unsafe { GcErased::from_raw(raw).map(|value| Gc::from_erased(value)) }
    }

    /// Overwrite the current ephemeron pair.
    pub fn set(&self, key: Weak<K>, value: Weak<V>) {
        self.key.store(Self::raw_value(key), Ordering::Release);
        self.value.store(Self::raw_value(value), Ordering::Release);
    }

    /// Clear the current ephemeron pair.
    pub fn clear(&self) {
        self.set(Weak::empty(), Weak::empty());
    }

    /// Visit the current ephemeron pair during fixpoint tracing.
    pub fn visit(&self, visitor: &mut dyn EphemeronVisitor) {
        if let (Some(key), Some(value)) = (self.key(), self.value()) {
            visitor.visit_ephemeron(key.erase(), value.erase());
        }
    }

    /// Process the current ephemeron pair against the collector liveness view.
    pub fn process(&self, processor: &mut dyn WeakProcessor) {
        let (Some(key), Some(value)) = (self.key(), self.value()) else {
            self.clear();
            return;
        };
        let Some(remapped_key) = processor.remap_or_drop(key.erase()) else {
            self.clear();
            return;
        };
        let Some(remapped_value) = processor.remap_or_drop(value.erase()) else {
            self.clear();
            return;
        };
        self.set(
            Weak::new(unsafe { Gc::from_erased(remapped_key) }),
            Weak::new(unsafe { Gc::from_erased(remapped_value) }),
        );
    }

    fn raw_value<T: ?Sized>(value: Weak<T>) -> *mut ObjectHeader {
        match value.target() {
            Some(target) => target.erase().as_raw(),
            None => core::ptr::null_mut(),
        }
    }
}

impl<K: ?Sized, V: ?Sized> Default for Ephemeron<K, V> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<K: ?Sized, V: ?Sized> fmt::Debug for Ephemeron<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ephemeron")
            .field("has_key", &self.key().is_some())
            .field("has_value", &self.value().is_some())
            .finish()
    }
}

#[cfg(test)]
#[path = "weak_test.rs"]
mod tests;
