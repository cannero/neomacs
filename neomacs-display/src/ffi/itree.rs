//! C FFI for the Rust interval tree.
//!
//! This provides a C-compatible API that matches the interface of Emacs's
//! `itree.h`, allowing the Rust implementation to be used as a drop-in
//! replacement for the C `itree.c`.
//!
//! Key types:
//! - `RustItreeTree` → opaque pointer (Box<ItreeTree>)
//! - `RustItreeNode` → u64 handle encoding a NodeId
//! - `RustItreeIterator` → stack-allocated iterator state

use std::ffi::{c_int, c_void};
use std::ptr;

use crate::core::itree::{ItreeOrder, ItreeTree, NodeId};

/// Opaque tree handle for C.
pub type RustItreeTree = c_void;

/// Node handle for C — encodes a NodeId as u64.
pub type RustItreeNodeHandle = u64;

/// Iterator handle for C.
pub type RustItreeIterator = c_void;

const INVALID_HANDLE: u64 = u64::MAX;

fn handle_from_node_id(id: NodeId) -> RustItreeNodeHandle {
    id.index() as u64
}

fn node_id_from_handle(handle: RustItreeNodeHandle) -> Option<NodeId> {
    if handle == INVALID_HANDLE {
        None
    } else {
        Some(NodeId(handle as u32))
    }
}

fn order_from_c(order: c_int) -> ItreeOrder {
    match order {
        0 => ItreeOrder::Ascending,
        1 => ItreeOrder::Descending,
        2 => ItreeOrder::PreOrder,
        3 => ItreeOrder::PostOrder,
        _ => ItreeOrder::Ascending,
    }
}

/// Persistent iterator state that C code can hold.
struct IteratorState {
    begin: i64,
    end: i64,
    otick: u64,
    order: ItreeOrder,
    node: Option<NodeId>,
}

// ============================================================================
// Tree lifecycle
// ============================================================================

/// Create a new interval tree. Returns an opaque handle.
#[unsafe(no_mangle)]
pub extern "C" fn rust_itree_create() -> *mut RustItreeTree {
    let tree = Box::new(ItreeTree::new());
    Box::into_raw(tree) as *mut RustItreeTree
}

/// Destroy an interval tree. The tree must be empty (all nodes removed).
///
/// # Safety
/// `tree` must be a valid pointer returned by `rust_itree_create`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_destroy(tree: *mut RustItreeTree) {
    if tree.is_null() {
        return;
    }
    let _ = Box::from_raw(tree as *mut ItreeTree);
}

/// Clear the tree (reset to empty state).
///
/// # Safety
/// `tree` must be a valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_clear(tree: *mut RustItreeTree) {
    if tree.is_null() {
        return;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    tree.clear();
}

/// Return the number of nodes in the tree.
///
/// # Safety
/// `tree` must be a valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_size(tree: *mut RustItreeTree) -> i64 {
    if tree.is_null() {
        return 0;
    }
    let tree = &*(tree as *const ItreeTree);
    tree.size()
}

/// Return 1 if the tree is empty, 0 otherwise.
///
/// # Safety
/// `tree` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_empty_p(tree: *mut RustItreeTree) -> c_int {
    if tree.is_null() {
        return 1;
    }
    let tree = &*(tree as *const ItreeTree);
    tree.is_empty() as c_int
}

// ============================================================================
// Node lifecycle
// ============================================================================

/// Allocate and initialize a new node. Returns a node handle.
///
/// # Safety
/// `tree` must be a valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_node_create(
    tree: *mut RustItreeTree,
    front_advance: c_int,
    rear_advance: c_int,
    data: u64,
) -> RustItreeNodeHandle {
    if tree.is_null() {
        return INVALID_HANDLE;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    let id = tree.alloc_node(front_advance != 0, rear_advance != 0, data);
    handle_from_node_id(id)
}

/// Free a node back to the arena.
///
/// # Safety
/// `tree` must be valid. `node` must be a valid handle for a node not in the tree.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_node_destroy(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
) {
    if tree.is_null() {
        return;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    if let Some(id) = node_id_from_handle(node) {
        tree.free_node(id);
    }
}

// ============================================================================
// Node field accessors
// ============================================================================

/// Get a node's begin position (validates/computes lazy offsets).
///
/// # Safety
/// `tree` and `node` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_node_begin(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
) -> i64 {
    if tree.is_null() {
        return 0;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    let id = match node_id_from_handle(node) {
        Some(id) => id,
        None => return 0,
    };
    tree.node_begin(id)
}

/// Get a node's end position (validates/computes lazy offsets).
///
/// # Safety
/// `tree` and `node` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_node_end(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
) -> i64 {
    if tree.is_null() {
        return 0;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    let id = match node_id_from_handle(node) {
        Some(id) => id,
        None => return 0,
    };
    tree.node_end(id)
}

/// Get a node's data field.
///
/// # Safety
/// `tree` and `node` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_node_data(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
) -> u64 {
    if tree.is_null() {
        return 0;
    }
    let tree = &*(tree as *const ItreeTree);
    let id = match node_id_from_handle(node) {
        Some(id) => id,
        None => return 0,
    };
    tree.node(id).data
}

/// Get a node's front_advance flag.
///
/// # Safety
/// `tree` and `node` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_node_front_advance(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
) -> c_int {
    if tree.is_null() {
        return 0;
    }
    let tree = &*(tree as *const ItreeTree);
    let id = match node_id_from_handle(node) {
        Some(id) => id,
        None => return 0,
    };
    tree.node(id).front_advance as c_int
}

/// Get a node's rear_advance flag.
///
/// # Safety
/// `tree` and `node` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_node_rear_advance(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
) -> c_int {
    if tree.is_null() {
        return 0;
    }
    let tree = &*(tree as *const ItreeTree);
    let id = match node_id_from_handle(node) {
        Some(id) => id,
        None => return 0,
    };
    tree.node(id).rear_advance as c_int
}

// ============================================================================
// Tree operations
// ============================================================================

/// Insert a node into the tree with the given range.
///
/// # Safety
/// `tree` must be valid. `node` must be a valid handle not currently in the tree.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_insert(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
    begin: i64,
    end: i64,
) {
    if tree.is_null() {
        return;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    let id = match node_id_from_handle(node) {
        Some(id) => id,
        None => return,
    };
    tree.insert(id, begin, end);
}

/// Remove a node from the tree. Returns the node handle.
///
/// # Safety
/// `tree` must be valid. `node` must be in the tree.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_remove(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
) -> RustItreeNodeHandle {
    if tree.is_null() {
        return INVALID_HANDLE;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    let id = match node_id_from_handle(node) {
        Some(id) => id,
        None => return INVALID_HANDLE,
    };
    let removed = tree.remove(id);
    handle_from_node_id(removed)
}

/// Set a node's region (begin, end). Handles rebalancing if begin changes.
///
/// # Safety
/// `tree` must be valid. `node` must be in the tree.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_node_set_region(
    tree: *mut RustItreeTree,
    node: RustItreeNodeHandle,
    begin: i64,
    end: i64,
) {
    if tree.is_null() {
        return;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    let id = match node_id_from_handle(node) {
        Some(id) => id,
        None => return,
    };
    tree.node_set_region(id, begin, end);
}

/// Insert a gap at `pos` of `length`.
///
/// # Safety
/// `tree` must be valid or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_insert_gap(
    tree: *mut RustItreeTree,
    pos: i64,
    length: i64,
    before_markers: c_int,
) {
    if tree.is_null() {
        return;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    tree.insert_gap(pos, length, before_markers != 0);
}

/// Delete a gap at `pos` of `length`.
///
/// # Safety
/// `tree` must be valid or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_delete_gap(
    tree: *mut RustItreeTree,
    pos: i64,
    length: i64,
) {
    if tree.is_null() {
        return;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    tree.delete_gap(pos, length);
}

// ============================================================================
// Iterator
// ============================================================================

/// Start an iterator. Returns an opaque iterator handle.
/// The iterator must be destroyed with `rust_itree_iterator_destroy`.
///
/// # Safety
/// `tree` must be valid and non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_iterator_start(
    tree: *mut RustItreeTree,
    begin: i64,
    end: i64,
    order: c_int,
) -> *mut RustItreeIterator {
    if tree.is_null() {
        return ptr::null_mut();
    }
    let tree = &mut *(tree as *mut ItreeTree);
    let iter = tree.iterator_start(begin, end, order_from_c(order));

    let state = Box::new(IteratorState {
        begin: iter.begin,
        end: iter.end,
        otick: iter.otick,
        order: iter.order,
        node: iter.node,
    });
    Box::into_raw(state) as *mut RustItreeIterator
}

/// Get the next node from the iterator. Returns INVALID_HANDLE when done.
///
/// # Safety
/// `tree` and `iter` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_iterator_next(
    tree: *mut RustItreeTree,
    iter: *mut RustItreeIterator,
) -> RustItreeNodeHandle {
    if tree.is_null() || iter.is_null() {
        return INVALID_HANDLE;
    }
    let tree = &mut *(tree as *mut ItreeTree);
    let state = &mut *(iter as *mut IteratorState);

    // Reconstruct the iterator from state
    let mut rust_iter = crate::core::itree::ItreeIterator {
        begin: state.begin,
        end: state.end,
        otick: state.otick,
        order: state.order,
        node: state.node,
    };

    let result = tree.iterator_next(&mut rust_iter);

    // Save state back
    state.begin = rust_iter.begin;
    state.end = rust_iter.end;
    state.otick = rust_iter.otick;
    state.order = rust_iter.order;
    state.node = rust_iter.node;

    match result {
        Some(id) => handle_from_node_id(id),
        None => INVALID_HANDLE,
    }
}

/// Narrow the iterator to a subset of its current range.
///
/// # Safety
/// `iter` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_iterator_narrow(
    iter: *mut RustItreeIterator,
    begin: i64,
    end: i64,
) {
    if iter.is_null() {
        return;
    }
    let state = &mut *(iter as *mut IteratorState);
    if begin > state.begin {
        state.begin = begin;
    }
    if end < state.end {
        state.end = end;
    }
}

/// Destroy an iterator.
///
/// # Safety
/// `iter` must have been returned by `rust_itree_iterator_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_itree_iterator_destroy(iter: *mut RustItreeIterator) {
    if iter.is_null() {
        return;
    }
    let _ = Box::from_raw(iter as *mut IteratorState);
}
