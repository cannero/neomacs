use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::descriptor::{GcErased, Relocator};
use crate::heap::Heap;
use crate::object::ObjectHeader;

/// Unrooted managed pointer.
#[derive(Debug)]
pub struct Gc<T: ?Sized> {
    object: GcErased,
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> Gc<T> {
    pub(crate) unsafe fn from_erased(object: GcErased) -> Self {
        Self {
            object,
            _marker: PhantomData,
        }
    }

    /// Erase the concrete payload type for tracing and collector APIs.
    pub fn erase(self) -> GcErased {
        self.object
    }
}

impl<T> Gc<T> {
    /// Return the raw non-null payload address.
    pub fn as_non_null(self) -> NonNull<T> {
        let payload = unsafe { ObjectHeader::payload_ptr(self.object.header()) };
        payload.cast()
    }
}

impl<T: ?Sized> Copy for Gc<T> {}

impl<T: ?Sized> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        self.object == other.object
    }
}

impl<T: ?Sized> Eq for Gc<T> {}

impl<T: ?Sized> Hash for Gc<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.object.hash(state);
    }
}

/// Rooted managed handle that survives collection.
#[derive(Debug)]
pub struct Root<'scope, T: ?Sized> {
    root_stack: NonNull<RootStack>,
    index: usize,
    _scope: PhantomData<&'scope T>,
}

impl<'scope, T: ?Sized> Root<'scope, T> {
    pub(crate) fn new(root_stack: NonNull<RootStack>, index: usize) -> Self {
        Self {
            root_stack,
            index,
            _scope: PhantomData,
        }
    }

    /// Return the rooted pointer as a plain managed reference.
    pub fn as_gc(&self) -> Gc<T> {
        let object = unsafe {
            self.root_stack
                .as_ref()
                .get(self.index)
                .expect("root slot missing while handle is live")
        };
        unsafe { Gc::from_erased(object) }
    }
}

#[derive(Debug)]
pub(crate) struct HandleScopeState<'heap> {
    heap: &'heap Heap,
    depth: usize,
    safepoint: Option<std::sync::RwLockReadGuard<'heap, ()>>,
}

impl<'heap> HandleScopeState<'heap> {
    pub(crate) fn new(heap: &'heap Heap) -> Self {
        Self {
            heap,
            depth: 0,
            safepoint: None,
        }
    }

    pub(crate) fn begin_scope(&mut self) {
        self.depth = self.depth.saturating_add(1);
        self.ensure_safepoint();
    }

    pub(crate) fn ensure_safepoint(&mut self) {
        if self.depth > 0 && self.safepoint.is_none() {
            self.safepoint = Some(self.heap.read_safepoint());
        }
    }

    pub(crate) fn has_safepoint(&self) -> bool {
        self.safepoint.is_some()
    }

    pub(crate) fn release_safepoint(&mut self) {
        self.safepoint = None;
    }

    fn end_scope(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 {
            self.safepoint = None;
        }
    }
}

/// Scope that owns a transient stack of roots.
#[derive(Debug)]
pub struct HandleScope<'scope, 'heap> {
    root_stack: NonNull<RootStack>,
    scope_state: Option<NonNull<HandleScopeState<'heap>>>,
    start: usize,
    _marker: PhantomData<&'scope mut &'heap mut RootStack>,
}

impl<'scope, 'heap> HandleScope<'scope, 'heap> {
    pub(crate) fn new(root_stack: NonNull<RootStack>) -> Self {
        let start = unsafe { root_stack.as_ref().len() };
        Self {
            root_stack,
            scope_state: None,
            start,
            _marker: PhantomData,
        }
    }

    pub(crate) fn new_with_state(
        root_stack: NonNull<RootStack>,
        scope_state: NonNull<HandleScopeState<'heap>>,
    ) -> Self {
        let start = unsafe { root_stack.as_ref().len() };
        Self {
            root_stack,
            scope_state: Some(scope_state),
            start,
            _marker: PhantomData,
        }
    }

    /// Number of root slots created in this scope.
    pub fn slot_count(&self) -> usize {
        unsafe { self.root_stack.as_ref().len().saturating_sub(self.start) }
    }

    pub(crate) fn root<T: ?Sized>(&mut self, gc: Gc<T>) -> Root<'scope, T> {
        let index = unsafe { self.root_stack.as_mut().push(gc.erase()) };
        Root::new(self.root_stack, index)
    }
}

impl<'scope, 'heap> Drop for HandleScope<'scope, 'heap> {
    fn drop(&mut self) {
        unsafe {
            self.root_stack.as_mut().truncate(self.start);
        }
        if let Some(mut scope_state) = self.scope_state {
            unsafe { scope_state.as_mut().end_scope() };
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct RootStack {
    slots: Vec<RootSlot>,
}

impl RootStack {
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn push(&mut self, object: GcErased) -> usize {
        let index = self.slots.len();
        self.slots.push(RootSlot::new(Some(object)));
        index
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.slots.truncate(len);
    }

    pub(crate) fn get(&self, index: usize) -> Option<GcErased> {
        self.slots.get(index).and_then(RootSlot::get)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = GcErased> + '_ {
        self.slots.iter().filter_map(RootSlot::get)
    }

    pub(crate) fn relocate_all(&mut self, relocator: &mut dyn Relocator) {
        for slot in &self.slots {
            if let Some(object) = slot.get() {
                let relocated = relocator.relocate_erased(object);
                slot.set(Some(relocated));
            }
        }
    }
}

#[derive(Debug)]
struct RootSlot {
    object: AtomicPtr<ObjectHeader>,
}

impl RootSlot {
    fn new(object: Option<GcErased>) -> Self {
        Self {
            object: AtomicPtr::new(match object {
                Some(object) => object.as_raw(),
                None => core::ptr::null_mut(),
            }),
        }
    }

    pub(crate) fn get(&self) -> Option<GcErased> {
        let raw = self.object.load(Ordering::Acquire);
        unsafe { GcErased::from_raw(raw) }
    }

    pub(crate) fn set(&self, object: Option<GcErased>) {
        self.object.store(
            match object {
                Some(object) => object.as_raw(),
                None => core::ptr::null_mut(),
            },
            Ordering::Release,
        );
    }
}

#[cfg(test)]
#[path = "root_test.rs"]
mod tests;
