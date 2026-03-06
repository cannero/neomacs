//! Augmented interval tree with lazy offset propagation.
//!
//! This is a Rust implementation of the interval tree data structure used by
//! Emacs for overlay management. It stores half-open intervals `[begin, end)`
//! in a Red-Black tree ordered by `begin`, with an augmented `limit` field
//! (the maximum `end` in each subtree) for efficient intersection queries.
//!
//! Key features:
//! - O(log N) insert/remove
//! - O(K log N) intersection queries (K = result size)
//! - O(log N) amortized gap insert/delete (for buffer text changes)
//! - Lazy offset propagation via otick/offset scheme
//!
//! # Arena-based allocation
//!
//! Nodes are stored in a `Vec`-based arena and referenced by `NodeId` indices.
//! This avoids raw pointers and gives full memory safety within the tree
//! algorithms.

use std::cmp::{max, min};

/// Index into the node arena. `u32` supports up to ~4 billion nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Traversal order for the iterator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItreeOrder {
    Ascending,
    Descending,
    PreOrder,
    PostOrder,
}

/// A node in the interval tree.
#[derive(Debug, Clone)]
pub struct ItreeNode {
    // Tree structure
    pub parent: Option<NodeId>,
    pub left: Option<NodeId>,
    pub right: Option<NodeId>,

    // Interval fields
    pub begin: i64,
    pub end: i64,
    pub limit: i64,
    pub offset: i64,
    pub otick: u64,

    // Client data (opaque, e.g. Lisp_Object for overlays)
    pub data: u64,

    // Node properties
    pub red: bool,
    pub rear_advance: bool,
    pub front_advance: bool,

    // Arena management: false if this slot has been freed
    alive: bool,
}

impl ItreeNode {
    fn new(front_advance: bool, rear_advance: bool, data: u64) -> Self {
        ItreeNode {
            parent: None,
            left: None,
            right: None,
            begin: -1,
            end: -1,
            limit: i64::MIN,
            offset: 0,
            otick: 0,
            data,
            red: false,
            rear_advance,
            front_advance,
            alive: true,
        }
    }
}

/// An augmented interval tree with Red-Black balancing and lazy offset propagation.
pub struct ItreeTree {
    /// Node arena. Indices are `NodeId`.
    nodes: Vec<ItreeNode>,
    /// Free list for recycling node slots.
    free_list: Vec<u32>,
    /// Root node of the RB tree.
    root: Option<NodeId>,
    /// Global offset tick. Nodes with `otick < tree.otick` may have pending offsets.
    pub otick: u64,
    /// Number of live nodes in the tree.
    size: i64,
}

impl ItreeTree {
    /// Create a new empty interval tree.
    pub fn new() -> Self {
        ItreeTree {
            nodes: Vec::new(),
            free_list: Vec::new(),
            root: None,
            otick: 1,
            size: 0,
        }
    }

    /// Reset the tree to empty state. Nodes are NOT deallocated from the arena.
    pub fn clear(&mut self) {
        self.root = None;
        self.otick = 1;
        self.size = 0;
        // Mark all nodes as dead
        for node in &mut self.nodes {
            node.alive = false;
        }
        self.free_list.clear();
        for i in (0..self.nodes.len()).rev() {
            self.free_list.push(i as u32);
        }
    }

    /// Return the number of nodes in the tree.
    pub fn size(&self) -> i64 {
        self.size
    }

    /// Return true if the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    // ===== Arena operations =====

    /// Allocate a new node in the arena and return its ID.
    pub fn alloc_node(&mut self, front_advance: bool, rear_advance: bool, data: u64) -> NodeId {
        if let Some(idx) = self.free_list.pop() {
            let node = &mut self.nodes[idx as usize];
            *node = ItreeNode::new(front_advance, rear_advance, data);
            NodeId(idx)
        } else {
            let idx = self.nodes.len() as u32;
            self.nodes
                .push(ItreeNode::new(front_advance, rear_advance, data));
            NodeId(idx)
        }
    }

    /// Free a node back to the arena.
    pub fn free_node(&mut self, id: NodeId) {
        self.nodes[id.index()].alive = false;
        self.free_list.push(id.0);
    }

    /// Get an immutable reference to a node.
    #[inline(always)]
    pub fn node(&self, id: NodeId) -> &ItreeNode {
        debug_assert!(self.nodes[id.index()].alive);
        &self.nodes[id.index()]
    }

    /// Get a mutable reference to a node.
    #[inline(always)]
    fn node_mut(&mut self, id: NodeId) -> &mut ItreeNode {
        debug_assert!(self.nodes[id.index()].alive);
        &mut self.nodes[id.index()]
    }

    // ===== Field accessors (avoid borrow issues) =====

    #[inline(always)]
    fn begin(&self, id: NodeId) -> i64 {
        self.nodes[id.index()].begin
    }
    #[inline(always)]
    fn end(&self, id: NodeId) -> i64 {
        self.nodes[id.index()].end
    }
    #[inline(always)]
    fn limit(&self, id: NodeId) -> i64 {
        self.nodes[id.index()].limit
    }
    #[inline(always)]
    fn offset(&self, id: NodeId) -> i64 {
        self.nodes[id.index()].offset
    }
    #[inline(always)]
    fn otick_of(&self, id: NodeId) -> u64 {
        self.nodes[id.index()].otick
    }
    #[inline(always)]
    fn is_red(&self, id: NodeId) -> bool {
        self.nodes[id.index()].red
    }
    #[inline(always)]
    fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].parent
    }
    #[inline(always)]
    fn left(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].left
    }
    #[inline(always)]
    fn right(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].right
    }
    #[inline(always)]
    fn front_advance(&self, id: NodeId) -> bool {
        self.nodes[id.index()].front_advance
    }
    #[inline(always)]
    fn rear_advance(&self, id: NodeId) -> bool {
        self.nodes[id.index()].rear_advance
    }

    // ===== Internal helpers =====

    fn null_safe_is_red(&self, id: Option<NodeId>) -> bool {
        id.map_or(false, |n| self.is_red(n))
    }

    fn null_safe_is_black(&self, id: Option<NodeId>) -> bool {
        id.map_or(true, |n| !self.is_red(n))
    }

    /// Compute the limit for a node based on its end and children's limits.
    fn compute_limit(&self, id: NodeId) -> i64 {
        let node_end = self.end(id);
        let left_limit = self
            .left(id)
            .map_or(i64::MIN, |l| self.limit(l) + self.offset(l));
        let right_limit = self
            .right(id)
            .map_or(i64::MIN, |r| self.limit(r) + self.offset(r));
        max(node_end, max(left_limit, right_limit))
    }

    /// Update a node's limit from its children.
    fn update_limit(&mut self, id: NodeId) {
        let new_limit = self.compute_limit(id);
        self.node_mut(id).limit = new_limit;
    }

    /// Propagate offset from a node to its children and clean the node.
    fn inherit_offset(&mut self, otick: u64, id: NodeId) {
        if self.otick_of(id) == otick {
            debug_assert!(self.offset(id) == 0);
            return;
        }

        let offset = self.offset(id);
        if offset != 0 {
            let n = &mut self.nodes[id.index()];
            n.begin += offset;
            n.end += offset;
            n.limit += offset;
            n.offset = 0;

            let left = n.left;
            let right = n.right;

            if let Some(l) = left {
                self.nodes[l.index()].offset += offset;
            }
            if let Some(r) = right {
                self.nodes[r.index()].offset += offset;
            }
        }

        let parent = self.parent(id);
        if parent.is_none() || parent.map_or(false, |p| self.otick_of(p) == otick) {
            self.node_mut(id).otick = otick;
        }
    }

    /// Propagate limit up to the root until stable.
    fn propagate_limit(&mut self, start: Option<NodeId>) {
        let mut current = match start {
            Some(id) => id,
            None => return,
        };

        loop {
            let new_limit = self.compute_limit(current);
            if new_limit == self.limit(current) {
                break;
            }
            self.node_mut(current).limit = new_limit;
            match self.parent(current) {
                Some(p) => current = p,
                None => break,
            }
        }
    }

    /// Validate a node by propagating offsets from root down to it.
    fn validate(&mut self, id: NodeId) {
        if self.otick == self.otick_of(id) {
            return;
        }
        if let Some(parent) = self.parent(id) {
            if Some(id) != self.root {
                self.validate(parent);
            }
        }
        let otick = self.otick;
        self.inherit_offset(otick, id);
    }

    /// Return NODE's begin value, computing it if necessary.
    pub fn node_begin(&mut self, id: NodeId) -> i64 {
        self.validate(id);
        self.begin(id)
    }

    /// Return NODE's end value, computing it if necessary.
    pub fn node_end(&mut self, id: NodeId) -> i64 {
        self.validate(id);
        self.end(id)
    }

    // ===== Rotations =====

    /// Left rotation on `node`.
    fn rotate_left(&mut self, node: NodeId) {
        let right = match self.right(node) {
            Some(r) => r,
            None => return, // structural invariant violated; skip rotation
        };

        let otick = self.otick;
        self.inherit_offset(otick, node);
        self.inherit_offset(otick, right);

        // Turn right's left subtree into node's right subtree
        let right_left = self.left(right);
        self.node_mut(node).right = right_left;
        if let Some(rl) = right_left {
            self.node_mut(rl).parent = Some(node);
        }

        // right's parent was node's parent
        let node_parent = self.parent(node);
        self.node_mut(right).parent = node_parent;

        // Get the parent to point to right instead of node
        if Some(node) != self.root {
            if let Some(parent) = node_parent {
                if self.left(parent) == Some(node) {
                    self.node_mut(parent).left = Some(right);
                } else {
                    self.node_mut(parent).right = Some(right);
                }
            }
        } else {
            self.root = Some(right);
        }

        // Put node on right's left
        self.node_mut(right).left = Some(node);
        self.node_mut(node).parent = Some(right);

        // Order matters: update node first, then right
        self.update_limit(node);
        self.update_limit(right);
    }

    /// Right rotation on `node`.
    fn rotate_right(&mut self, node: NodeId) {
        let left = match self.left(node) {
            Some(l) => l,
            None => return, // structural invariant violated; skip rotation
        };

        let otick = self.otick;
        self.inherit_offset(otick, node);
        self.inherit_offset(otick, left);

        // Turn left's right subtree into node's left subtree
        let left_right = self.right(left);
        self.node_mut(node).left = left_right;
        if let Some(lr) = left_right {
            self.node_mut(lr).parent = Some(node);
        }

        // left's parent was node's parent
        let node_parent = self.parent(node);
        self.node_mut(left).parent = node_parent;

        if Some(node) != self.root {
            if let Some(parent) = node_parent {
                if self.right(parent) == Some(node) {
                    self.node_mut(parent).right = Some(left);
                } else {
                    self.node_mut(parent).left = Some(left);
                }
            }
        } else {
            self.root = Some(left);
        }

        self.node_mut(left).right = Some(node);
        self.node_mut(node).parent = Some(left);

        self.update_limit(left);
        self.update_limit(node);
    }

    // ===== Insert =====

    /// Fix red-black violations after insertion.
    fn insert_fix(&mut self, mut node: NodeId) {
        while self.null_safe_is_red(self.parent(node)) {
            debug_assert!(self.is_red(node));
            let parent = match self.parent(node) {
                Some(p) => p,
                None => break,
            };
            let grandparent = match self.parent(parent) {
                Some(gp) => gp,
                None => break,
            };

            if Some(parent) == self.left(grandparent) {
                let uncle = self.right(grandparent);

                if self.null_safe_is_red(uncle) {
                    // Case 1a: uncle is red
                    self.node_mut(parent).red = false;
                    if let Some(u) = uncle {
                        self.node_mut(u).red = false;
                    }
                    self.node_mut(grandparent).red = true;
                    node = grandparent;
                } else {
                    if Some(node) == self.right(parent) {
                        // Case 2a
                        node = parent;
                        self.rotate_left(node);
                    }
                    // Case 3a
                    if let Some(parent) = self.parent(node) {
                        if let Some(grandparent) = self.parent(parent) {
                            self.node_mut(parent).red = false;
                            self.node_mut(grandparent).red = true;
                            self.rotate_right(grandparent);
                        }
                    }
                }
            } else {
                // Symmetric case
                let uncle = self.left(grandparent);

                if self.null_safe_is_red(uncle) {
                    // Case 1b
                    self.node_mut(parent).red = false;
                    if let Some(u) = uncle {
                        self.node_mut(u).red = false;
                    }
                    self.node_mut(grandparent).red = true;
                    node = grandparent;
                } else {
                    if Some(node) == self.left(parent) {
                        // Case 2b
                        node = parent;
                        self.rotate_right(node);
                    }
                    // Case 3b
                    if let Some(parent) = self.parent(node) {
                        if let Some(grandparent) = self.parent(parent) {
                            self.node_mut(parent).red = false;
                            self.node_mut(grandparent).red = true;
                            self.rotate_left(grandparent);
                        }
                    }
                }
            }
        }

        // Root must be black
        if let Some(root) = self.root {
            self.node_mut(root).red = false;
        }
    }

    /// Insert a node into the tree (internal, node must have begin/end/otick set).
    fn insert_node(&mut self, node: NodeId) {
        debug_assert!(self.begin(node) <= self.end(node));
        debug_assert!(self.left(node).is_none());
        debug_assert!(self.right(node).is_none());
        debug_assert!(self.parent(node).is_none());
        debug_assert!(self.otick_of(node) == self.otick);

        let otick = self.otick;
        let node_begin = self.begin(node);
        let node_end = self.end(node);

        // Find insertion point
        let mut parent: Option<NodeId> = None;
        let mut current = self.root;

        while let Some(child) = current {
            self.inherit_offset(otick, child);
            parent = Some(child);
            // Update limit of ancestors
            let child_limit = self.limit(child);
            if node_end > child_limit {
                self.node_mut(child).limit = node_end;
            }
            current = if node_begin <= self.begin(child) {
                self.left(child)
            } else {
                self.right(child)
            };
        }

        // Insert
        if let Some(p) = parent {
            if node_begin <= self.begin(p) {
                self.node_mut(p).left = Some(node);
            } else {
                self.node_mut(p).right = Some(node);
            }
        } else {
            self.root = Some(node);
        }

        let n = &mut self.nodes[node.index()];
        n.parent = parent;
        n.left = None;
        n.right = None;
        n.offset = 0;
        n.limit = node_end;

        self.size += 1;

        if self.root == Some(node) {
            self.node_mut(node).red = false;
        } else {
            self.node_mut(node).red = true;
            self.insert_fix(node);
        }
    }

    /// Insert a node with the given begin/end range.
    pub fn insert(&mut self, node: NodeId, begin: i64, end: i64) {
        {
            let n = &mut self.nodes[node.index()];
            n.begin = begin;
            n.end = end;
            n.otick = self.otick;
        }
        self.insert_node(node);
    }

    /// Safely modify a node's interval.
    pub fn node_set_region(&mut self, node: NodeId, begin: i64, end: i64) {
        self.validate(node);
        let cur_begin = self.begin(node);
        let cur_end = self.end(node);

        if begin != cur_begin {
            self.remove(node);
            let clamped_begin = min(begin, i64::MAX - 1);
            let clamped_end = max(clamped_begin, end);
            {
                let n = &mut self.nodes[node.index()];
                n.begin = clamped_begin;
                n.end = clamped_end;
            }
            self.insert_node(node);
        } else if end != cur_end {
            let node_begin = self.begin(node);
            self.node_mut(node).end = max(node_begin, end);
            self.propagate_limit(Some(node));
        }
    }

    // ===== Remove =====

    /// Find the minimum node in a subtree.
    fn subtree_min(&mut self, mut node: NodeId) -> NodeId {
        let otick = self.otick;
        loop {
            self.inherit_offset(otick, node);
            match self.left(node) {
                Some(left) => node = left,
                None => return node,
            }
        }
    }

    /// Replace dest with source as a child of dest's parent.
    fn replace_child(&mut self, source: Option<NodeId>, dest: NodeId) {
        if Some(dest) == self.root {
            self.root = source;
        } else if let Some(parent) = self.parent(dest) {
            if self.left(parent) == Some(dest) {
                self.node_mut(parent).left = source;
            } else {
                self.node_mut(parent).right = source;
            }
        }

        if let Some(s) = source {
            self.node_mut(s).parent = self.parent(dest);
        }
    }

    /// Replace dest with source in the tree (full transplant).
    fn transplant(&mut self, source: NodeId, dest: NodeId) {
        self.replace_child(Some(source), dest);

        let dest_left = self.left(dest);
        let dest_right = self.right(dest);
        let dest_red = self.is_red(dest);

        self.node_mut(source).left = dest_left;
        if let Some(l) = dest_left {
            self.node_mut(l).parent = Some(source);
        }
        self.node_mut(source).right = dest_right;
        if let Some(r) = dest_right {
            self.node_mut(r).parent = Some(source);
        }
        self.node_mut(source).red = dest_red;
    }

    /// Fix red-black violations after removal.
    fn remove_fix(&mut self, mut node: Option<NodeId>, mut parent: Option<NodeId>) {
        while parent.is_some() && self.null_safe_is_black(node) {
            let p = match parent {
                Some(p) => p,
                None => break,
            };
            debug_assert!(node == self.left(p) || node == self.right(p));

            if node == self.left(p) {
                let mut other = match self.right(p) {
                    Some(o) => o,
                    None => break,
                };

                if self.is_red(other) {
                    // Case 1a
                    self.node_mut(other).red = false;
                    self.node_mut(p).red = true;
                    self.rotate_left(p);
                    other = match self.right(p) {
                        Some(o) => o,
                        None => break,
                    };
                }

                if self.null_safe_is_black(self.left(other))
                    && self.null_safe_is_black(self.right(other))
                {
                    // Case 2a
                    self.node_mut(other).red = true;
                    node = Some(p);
                    parent = self.parent(p);
                } else {
                    if self.null_safe_is_black(self.right(other)) {
                        // Case 3a
                        if let Some(other_left) = self.left(other) {
                            self.node_mut(other_left).red = false;
                        }
                        self.node_mut(other).red = true;
                        self.rotate_right(other);
                        other = match self.right(p) {
                            Some(o) => o,
                            None => break,
                        };
                    }
                    // Case 4a
                    let p_red = self.is_red(p);
                    self.node_mut(other).red = p_red;
                    self.node_mut(p).red = false;
                    if let Some(other_right) = self.right(other) {
                        self.node_mut(other_right).red = false;
                    }
                    self.rotate_left(p);
                    node = self.root;
                    parent = None;
                }
            } else {
                // Symmetric case
                let mut other = match self.left(p) {
                    Some(o) => o,
                    None => break,
                };

                if self.is_red(other) {
                    // Case 1b
                    self.node_mut(other).red = false;
                    self.node_mut(p).red = true;
                    self.rotate_right(p);
                    other = match self.left(p) {
                        Some(o) => o,
                        None => break,
                    };
                }

                if self.null_safe_is_black(self.right(other))
                    && self.null_safe_is_black(self.left(other))
                {
                    // Case 2b
                    self.node_mut(other).red = true;
                    node = Some(p);
                    parent = self.parent(p);
                } else {
                    if self.null_safe_is_black(self.left(other)) {
                        // Case 3b
                        if let Some(other_right) = self.right(other) {
                            self.node_mut(other_right).red = false;
                        }
                        self.node_mut(other).red = true;
                        self.rotate_left(other);
                        other = match self.left(p) {
                            Some(o) => o,
                            None => break,
                        };
                    }
                    // Case 4b
                    let p_red = self.is_red(p);
                    self.node_mut(other).red = p_red;
                    self.node_mut(p).red = false;
                    if let Some(other_left) = self.left(other) {
                        self.node_mut(other_left).red = false;
                    }
                    self.rotate_right(p);
                    node = self.root;
                    parent = None;
                }
            }
        }

        if let Some(n) = node {
            self.node_mut(n).red = false;
        }
    }

    /// Remove a node from the tree. Returns the NodeId for convenience.
    pub fn remove(&mut self, node: NodeId) -> NodeId {
        let otick = self.otick;
        self.inherit_offset(otick, node);

        // Find splice: the node to actually splice out
        let splice = if self.left(node).is_none() || self.right(node).is_none() {
            node
        } else {
            // Both children exist; we checked right is Some above
            match self.right(node) {
                Some(right) => self.subtree_min(right),
                None => node,
            }
        };

        // Find subtree: the only child of splice
        let subtree = if self.left(splice).is_some() {
            self.left(splice)
        } else {
            self.right(splice)
        };

        // subtree_parent: where subtree will end up
        let subtree_parent = if self.parent(splice) != Some(node) {
            self.parent(splice)
        } else {
            Some(splice)
        };

        // Replace splice with subtree
        self.replace_child(subtree, splice);
        let removed_black = !self.is_red(splice);

        // If splice != node, transplant splice into node's position
        if splice != node {
            self.transplant(splice, node);

            if let Some(sp) = subtree_parent {
                self.propagate_limit(Some(sp));
            }
            if Some(splice) != subtree_parent {
                self.update_limit(splice);
            }
        }
        self.propagate_limit(self.parent(splice));

        self.size -= 1;

        if removed_black {
            self.remove_fix(subtree, subtree_parent);
        }

        // Clean up the removed node
        {
            let n = &mut self.nodes[node.index()];
            n.red = false;
            n.left = None;
            n.right = None;
            n.parent = None;
            n.limit = 0;
        }
        debug_assert!(self.otick_of(node) == self.otick);
        debug_assert!(self.offset(node) == 0);

        node
    }

    // ===== Gap operations =====

    fn max_height(&self) -> usize {
        if self.size <= 0 {
            return 2;
        }
        (2.0 * (self.size as f64 + 1.0).log2() + 0.5) as usize
    }

    /// Insert a gap at `pos` of `length`, expanding all intervals that intersect it.
    /// Respects `front_advance` and `rear_advance` settings on nodes.
    /// If `before_markers` is true, all overlays at `pos` are treated as if
    /// front_advance/rear_advance were true.
    pub fn insert_gap(&mut self, pos: i64, length: i64, before_markers: bool) {
        if length <= 0 || self.root.is_none() {
            return;
        }

        let ootick = self.otick;

        // Collect nodes that need to be removed and reinserted
        // (front_advance nodes starting exactly at pos)
        let mut saved: Vec<NodeId> = Vec::new();
        if !before_markers {
            let overlapping: Vec<NodeId> = self.iter_nodes(pos, pos + 1, ItreeOrder::PreOrder);
            for nid in overlapping {
                let begin = self.begin(nid);
                let end = self.end(nid);
                let fa = self.front_advance(nid);
                let ra = self.rear_advance(nid);
                if begin == pos && fa && (begin != end || ra) {
                    saved.push(nid);
                }
            }
        }

        // Remove saved nodes
        for &nid in &saved {
            self.remove(nid);
        }

        // Process remaining nodes using a stack-based traversal
        if let Some(root) = self.root {
            let mut stack: Vec<NodeId> = Vec::with_capacity(self.max_height() + 1);
            stack.push(root);

            while let Some(node) = stack.pop() {
                let otick = self.otick;
                self.inherit_offset(otick, node);

                if pos > self.limit(node) {
                    continue;
                }

                if let Some(right) = self.right(node) {
                    if self.begin(node) > pos {
                        // All nodes in right subtree shift by length
                        self.nodes[right.index()].offset += length;
                        self.otick += 1;
                    } else {
                        stack.push(right);
                    }
                }

                if let Some(left) = self.left(node) {
                    stack.push(left);
                }

                let node_begin = self.begin(node);
                if if before_markers {
                    node_begin >= pos
                } else {
                    node_begin > pos
                } {
                    self.node_mut(node).begin += length;
                }

                let node_end = self.end(node);
                if node_end > pos
                    || (node_end == pos && (before_markers || self.rear_advance(node)))
                {
                    self.node_mut(node).end += length;
                    self.propagate_limit(Some(node));
                }
            }
        }

        // Reinsert saved nodes
        let notick = self.otick;
        for nid in saved {
            debug_assert!(self.otick_of(nid) == ootick);
            debug_assert!(self.begin(nid) == pos);
            {
                let n = &mut self.nodes[nid.index()];
                n.begin += length;
                n.end += length;
                n.otick = notick;
            }
            self.insert_node(nid);
        }
    }

    /// Delete a gap at `pos` of `length`, contracting all intervals that intersect it.
    pub fn delete_gap(&mut self, pos: i64, length: i64) {
        if length <= 0 || self.root.is_none() {
            return;
        }

        let root = match self.root {
            Some(r) => r,
            None => return,
        };
        let mut stack: Vec<NodeId> = Vec::with_capacity(self.max_height() + 1);
        stack.push(root);

        while let Some(node) = stack.pop() {
            let otick = self.otick;
            self.inherit_offset(otick, node);

            if pos > self.limit(node) {
                continue;
            }

            if let Some(right) = self.right(node) {
                if self.begin(node) > pos + length {
                    // Shift right subtree left
                    self.nodes[right.index()].offset -= length;
                    self.otick += 1;
                } else {
                    stack.push(right);
                }
            }

            if let Some(left) = self.left(node) {
                stack.push(left);
            }

            let node_begin = self.begin(node);
            if pos < node_begin {
                self.node_mut(node).begin = max(pos, node_begin - length);
            }

            let node_end = self.end(node);
            if node_end > pos {
                self.node_mut(node).end = max(pos, node_end - length);
                self.propagate_limit(Some(node));
            }
        }
    }

    // ===== Iterator support =====

    /// Check if a node's interval intersects with [begin, end).
    /// Empty nodes at begin are included. When begin==end, non-empty
    /// nodes starting at begin or ending at end are excluded.
    fn node_intersects(&self, node: NodeId, begin: i64, end: i64) -> bool {
        let nb = self.begin(node);
        let ne = self.end(node);
        (begin < ne && nb < end) || (nb == ne && begin == nb)
    }

    /// Collect all node IDs intersecting [begin, end) in the given order.
    /// This allocates a Vec — for zero-allocation iteration, use `ItreeIterator`.
    fn iter_nodes(&mut self, begin: i64, end: i64, order: ItreeOrder) -> Vec<NodeId> {
        let mut result = Vec::new();
        let root = match self.root {
            Some(r) => r,
            None => return result,
        };

        let otick = self.otick;
        self.inherit_offset(otick, root);

        let mut iter = ItreeIterator {
            begin,
            end,
            otick: self.otick,
            order,
            node: None,
        };

        // Find first node
        iter.node = self.iterator_first_node(root, &iter);

        loop {
            // Skip non-intersecting nodes
            while let Some(n) = iter.node {
                if self.node_intersects(n, iter.begin, iter.end) {
                    break;
                }
                iter.node = self.iter_next_in_subtree(n, &iter);
            }

            match iter.node {
                Some(n) => {
                    result.push(n);
                    iter.node = self.iter_next_in_subtree(n, &iter);
                }
                None => break,
            }
        }

        result
    }

    /// Create an iterator over intervals intersecting [begin, end).
    pub fn iterator_start(&mut self, begin: i64, end: i64, order: ItreeOrder) -> ItreeIterator {
        let root = self.root;
        let otick = self.otick;

        let mut iter = ItreeIterator {
            begin,
            end,
            otick,
            order,
            node: None,
        };

        if let Some(r) = root {
            self.inherit_offset(otick, r);
            iter.node = self.iterator_first_node(r, &iter);
        }

        iter
    }

    /// Advance the iterator to the next intersecting node.
    pub fn iterator_next(&mut self, iter: &mut ItreeIterator) -> Option<NodeId> {
        let mut node = iter.node?;

        // Skip non-intersecting nodes
        while !self.node_intersects(node, iter.begin, iter.end) {
            match self.iter_next_in_subtree(node, iter) {
                Some(next) => node = next,
                None => {
                    iter.node = None;
                    return None;
                }
            }
        }

        // Advance iterator to next
        iter.node = self.iter_next_in_subtree(node, iter);
        Some(node)
    }

    /// Find the first node for the iterator based on traversal order.
    fn iterator_first_node(&mut self, root: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        match iter.order {
            ItreeOrder::Ascending => {
                // Start from leftmost reachable node
                self.ascending_first(root, iter)
            }
            ItreeOrder::Descending => {
                // Start from rightmost reachable node
                self.descending_first(root, iter)
            }
            ItreeOrder::PreOrder => {
                // Root is first
                Some(root)
            }
            ItreeOrder::PostOrder => {
                // Deepest left-then-right node
                self.post_order_first(root, iter)
            }
        }
    }

    fn ascending_first(&mut self, root: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        // Navigate to the leftmost node that could contain intersecting intervals
        let mut node = root;
        let otick = iter.otick;

        // Go as far left as possible while limit >= begin
        loop {
            match self.left(node) {
                Some(left) => {
                    self.inherit_offset(otick, left);
                    if iter.begin <= self.limit(left) {
                        node = left;
                    } else {
                        break;
                    }
                }
                None => break,
            }
        }

        // Now find the actual first via ascending traversal
        // Check if this node is past the end
        if self.begin(node) > iter.end {
            return None;
        }
        Some(node)
    }

    fn descending_first(&mut self, root: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        let mut node = root;
        let otick = iter.otick;

        // Go as far right as possible while begin <= end
        loop {
            if self.begin(node) <= iter.end {
                match self.right(node) {
                    Some(right) => {
                        self.inherit_offset(otick, right);
                        node = right;
                    }
                    None => break,
                }
            } else {
                break;
            }
        }

        Some(node)
    }

    fn post_order_first(&mut self, root: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        let mut node = root;
        let otick = iter.otick;

        loop {
            let left_child = self.left(node);
            let go_left = match left_child {
                Some(left) => {
                    self.inherit_offset(otick, left);
                    iter.begin <= self.limit(left)
                }
                None => false,
            };

            let right_child = self.right(node);
            let go_right = if !go_left {
                self.begin(node) <= iter.end
                    && match right_child {
                        Some(right) => {
                            self.inherit_offset(otick, right);
                            true
                        }
                        None => false,
                    }
            } else {
                false
            };

            if go_left {
                match left_child {
                    Some(left) => node = left,
                    None => break,
                }
            } else if go_right {
                match right_child {
                    Some(right) => node = right,
                    None => break,
                }
            } else {
                break;
            }
        }

        Some(node)
    }

    /// Get the next node in subtree traversal order.
    fn iter_next_in_subtree(&mut self, node: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        match iter.order {
            ItreeOrder::Ascending => self.ascending_next(node, iter),
            ItreeOrder::Descending => self.descending_next(node, iter),
            ItreeOrder::PreOrder => self.pre_order_next(node, iter),
            ItreeOrder::PostOrder => self.post_order_next(node, iter),
        }
    }

    fn ascending_next(&mut self, node: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        let otick = iter.otick;

        match self.right(node) {
            None => {
                // Go up until we come from a left child
                let mut current = node;
                loop {
                    match self.parent(current) {
                        Some(parent) => {
                            if self.right(parent) == Some(current) {
                                current = parent;
                            } else {
                                // We were left child, parent is next
                                let result = parent;
                                if self.begin(result) > iter.end {
                                    return None;
                                }
                                return Some(result);
                            }
                        }
                        None => return None,
                    }
                }
            }
            Some(right) => {
                // Go to leftmost node in right subtree
                let mut current = right;
                self.inherit_offset(otick, current);

                loop {
                    match self.left(current) {
                        Some(left) => {
                            self.inherit_offset(otick, left);
                            if iter.begin <= self.limit(left) {
                                current = left;
                            } else {
                                break;
                            }
                        }
                        None => break,
                    }
                }

                if self.begin(current) > iter.end {
                    return None;
                }
                Some(current)
            }
        }
    }

    fn descending_next(&mut self, node: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        let otick = iter.otick;

        let go_left = match self.left(node) {
            Some(left) => {
                self.inherit_offset(otick, left);
                self.limit(left) >= iter.begin
            }
            None => false,
        };

        if !go_left {
            // Go up until we come from a right child
            let mut current = node;
            loop {
                match self.parent(current) {
                    Some(parent) => {
                        if self.left(parent) == Some(current) {
                            current = parent;
                        } else {
                            return Some(parent);
                        }
                    }
                    None => return None,
                }
            }
        } else {
            // Go to rightmost in left subtree
            // go_left is true, so self.left(node) must be Some
            let mut current = match self.left(node) {
                Some(l) => l,
                None => return None,
            };

            loop {
                if self.begin(current) <= iter.end {
                    match self.right(current) {
                        Some(right) => {
                            self.inherit_offset(otick, right);
                            current = right;
                        }
                        None => break,
                    }
                } else {
                    break;
                }
            }

            Some(current)
        }
    }

    fn pre_order_next(&mut self, node: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        let otick = iter.otick;

        // Try left child first
        if let Some(left) = self.left(node) {
            self.inherit_offset(otick, left);
            if self.limit(left) >= iter.begin {
                return Some(left);
            }
        }

        // Try right child
        if self.begin(node) <= iter.end {
            if let Some(right) = self.right(node) {
                self.inherit_offset(otick, right);
                return Some(right);
            }
        }

        // Go up
        let mut current = node;
        loop {
            match self.parent(current) {
                Some(parent) => {
                    if self.right(parent) == Some(current) {
                        current = parent;
                    } else {
                        debug_assert!(self.left(parent) == Some(current));
                        current = parent;
                        if self.begin(current) <= iter.end {
                            if let Some(right) = self.right(current) {
                                self.inherit_offset(otick, right);
                                return Some(right);
                            }
                        }
                    }
                }
                None => return None,
            }
        }
    }

    fn post_order_next(&mut self, node: NodeId, iter: &ItreeIterator) -> Option<NodeId> {
        let otick = iter.otick;

        let parent = self.parent(node);
        match parent {
            None => None,
            Some(p) => {
                if self.right(p) == Some(node) {
                    // Was right child, parent is next
                    Some(p)
                } else {
                    // Was left child
                    debug_assert!(self.left(p) == Some(node));
                    let parent_node = p;

                    // Try right sibling subtree
                    let mut current = match self.right(parent_node) {
                        Some(r) if self.begin(parent_node) <= iter.end => r,
                        _ => return Some(parent_node),
                    };
                    self.inherit_offset(otick, current);

                    // Descend to deepest post-order node
                    loop {
                        let left_child = self.left(current);
                        let go_left = match left_child {
                            Some(left) => {
                                self.inherit_offset(otick, left);
                                iter.begin <= self.limit(left)
                            }
                            None => false,
                        };

                        let right_child = self.right(current);
                        let go_right = if !go_left {
                            self.begin(current) <= iter.end
                                && match right_child {
                                    Some(right) => {
                                        self.inherit_offset(otick, right);
                                        true
                                    }
                                    None => false,
                                }
                        } else {
                            false
                        };

                        if go_left {
                            match left_child {
                                Some(left) => current = left,
                                None => break,
                            }
                        } else if go_right {
                            match right_child {
                                Some(right) => current = right,
                                None => break,
                            }
                        } else {
                            break;
                        }
                    }

                    Some(current)
                }
            }
        }
    }

    // ===== Validation (debug) =====

    /// Validate the tree invariants. Returns true if valid.
    /// Panics with a descriptive message on violation.
    #[cfg(test)]
    pub fn check(&self) -> bool {
        assert!(self.size >= 0);
        assert!((self.size == 0) == self.root.is_none());

        if let Some(root) = self.root {
            assert!(self.parent(root).is_none());
            assert!(!self.is_red(root), "root must be black");
            let result = self.check_subtree(root, self.otick, 0, i64::MIN, i64::MAX);
            assert_eq!(result.0 as i64, self.size, "size mismatch");
        }
        true
    }

    /// Returns (size, limit, black_height) for the subtree.
    #[cfg(test)]
    fn check_subtree(
        &self,
        node: NodeId,
        tree_otick: u64,
        offset: i64,
        min_begin: i64,
        max_begin: i64,
    ) -> (usize, i64, usize) {
        let n = self.node(node);
        let effective_offset = offset + n.offset;
        let begin = n.begin + effective_offset;
        let end = n.end + effective_offset;
        let limit = n.limit + effective_offset;

        assert!(min_begin <= begin, "begin order violation");
        assert!(begin <= max_begin, "begin order violation (max)");
        assert!(end <= limit, "end > limit");

        // Check otick invariants
        assert!(n.otick <= tree_otick);
        if n.otick == tree_otick {
            assert_eq!(n.offset, 0, "clean node has non-zero offset");
        }

        let (left_size, left_limit, left_bh) = match n.left {
            Some(l) => {
                assert_eq!(self.parent(l), Some(node), "left child parent mismatch");
                self.check_subtree(l, tree_otick, effective_offset, min_begin, begin)
            }
            None => (0, i64::MIN, 0),
        };

        let (right_size, right_limit, right_bh) = match n.right {
            Some(r) => {
                assert_eq!(self.parent(r), Some(node), "right child parent mismatch");
                self.check_subtree(r, tree_otick, effective_offset, begin, max_begin)
            }
            None => (0, i64::MIN, 0),
        };

        assert!(left_limit <= limit, "left limit exceeds node limit");
        assert!(right_limit <= limit, "right limit exceeds node limit");
        assert_eq!(
            limit,
            max(end, max(left_limit, right_limit)),
            "limit not max of end and children"
        );

        // RB invariants
        assert_eq!(left_bh, right_bh, "black height mismatch");
        if n.red {
            if let Some(p) = n.parent {
                assert!(!self.is_red(p), "red node with red parent");
            }
        }

        let bh = if n.red { 0 } else { 1 } + left_bh;
        (1 + left_size + right_size, limit, bh)
    }
}

impl Default for ItreeTree {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator state for traversing the interval tree.
pub struct ItreeIterator {
    pub begin: i64,
    pub end: i64,
    pub otick: u64,
    pub order: ItreeOrder,
    pub node: Option<NodeId>,
}

impl ItreeIterator {
    /// Narrow the iterator to a subset of the current range.
    pub fn narrow(&mut self, begin: i64, end: i64) {
        self.begin = max(begin, self.begin);
        self.end = min(end, self.end);
    }
}

// ===== Tests =====

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_empty() {
        let tree = ItreeTree::new();
        assert!(tree.is_empty());
        assert_eq!(tree.size(), 0);
    }

    #[test]
    fn test_insert_single() {
        let mut tree = ItreeTree::new();
        let n = tree.alloc_node(false, false, 42);
        tree.insert(n, 10, 20);
        assert_eq!(tree.size(), 1);
        assert!(!tree.is_empty());
        assert!(tree.check());
    }

    #[test]
    fn test_insert_multiple() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        let b = tree.alloc_node(false, false, 2);
        let c = tree.alloc_node(false, false, 3);
        tree.insert(a, 10, 20);
        tree.insert(b, 5, 15);
        tree.insert(c, 25, 35);
        assert_eq!(tree.size(), 3);
        assert!(tree.check());
    }

    #[test]
    fn test_insert_many_maintains_invariants() {
        let mut tree = ItreeTree::new();
        let mut nodes = Vec::new();
        for i in 0..100 {
            let n = tree.alloc_node(false, false, i as u64);
            tree.insert(n, i * 3, i * 3 + 10);
            nodes.push(n);
            assert!(tree.check());
        }
        assert_eq!(tree.size(), 100);
    }

    #[test]
    fn test_remove_single() {
        let mut tree = ItreeTree::new();
        let n = tree.alloc_node(false, false, 42);
        tree.insert(n, 10, 20);
        tree.remove(n);
        assert!(tree.is_empty());
        assert_eq!(tree.size(), 0);
        assert!(tree.check());
    }

    #[test]
    fn test_remove_maintains_invariants() {
        let mut tree = ItreeTree::new();
        let mut nodes = Vec::new();
        for i in 0..50 {
            let n = tree.alloc_node(false, false, i as u64);
            tree.insert(n, i * 2, i * 2 + 5);
            nodes.push(n);
        }
        assert!(tree.check());

        // Remove in arbitrary order
        for &i in &[25, 0, 49, 12, 37, 5, 42, 18, 31, 8] {
            tree.remove(nodes[i]);
            assert!(tree.check());
        }
        assert_eq!(tree.size(), 40);
    }

    #[test]
    fn test_remove_all() {
        let mut tree = ItreeTree::new();
        let mut nodes = Vec::new();
        for i in 0..30 {
            let n = tree.alloc_node(false, false, i as u64);
            tree.insert(n, i * 5, i * 5 + 10);
            nodes.push(n);
        }

        for n in nodes {
            tree.remove(n);
            assert!(tree.check());
        }
        assert!(tree.is_empty());
    }

    #[test]
    fn test_node_begin_end() {
        let mut tree = ItreeTree::new();
        let n = tree.alloc_node(false, false, 0);
        tree.insert(n, 42, 100);
        assert_eq!(tree.node_begin(n), 42);
        assert_eq!(tree.node_end(n), 100);
    }

    #[test]
    fn test_node_set_region() {
        let mut tree = ItreeTree::new();
        let n = tree.alloc_node(false, false, 0);
        tree.insert(n, 10, 20);
        assert!(tree.check());

        // Change begin (requires remove+reinsert)
        tree.node_set_region(n, 5, 20);
        assert_eq!(tree.node_begin(n), 5);
        assert_eq!(tree.node_end(n), 20);
        assert!(tree.check());

        // Change only end (in-place)
        tree.node_set_region(n, 5, 30);
        assert_eq!(tree.node_begin(n), 5);
        assert_eq!(tree.node_end(n), 30);
        assert!(tree.check());
    }

    #[test]
    fn test_insert_gap_basic() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        let b = tree.alloc_node(false, false, 2);
        tree.insert(a, 10, 20);
        tree.insert(b, 30, 40);
        assert!(tree.check());

        // Insert gap at position 15, length 5
        tree.insert_gap(15, 5, false);

        // 'a' straddles the gap: begin stays, end moves
        assert_eq!(tree.node_begin(a), 10);
        assert_eq!(tree.node_end(a), 25);
        // 'b' is entirely after the gap: both move
        assert_eq!(tree.node_begin(b), 35);
        assert_eq!(tree.node_end(b), 45);
    }

    #[test]
    fn test_insert_gap_at_begin() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        tree.insert(a, 10, 20);

        // Insert at exact begin, no front_advance
        tree.insert_gap(10, 5, false);
        // begin should NOT move (front_advance=false)
        assert_eq!(tree.node_begin(a), 10);
        assert_eq!(tree.node_end(a), 25);
    }

    #[test]
    fn test_insert_gap_front_advance() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(true, true, 1); // front_advance=true
        tree.insert(a, 10, 20);

        tree.insert_gap(10, 5, false);
        // front_advance: begin moves too
        assert_eq!(tree.node_begin(a), 15);
        assert_eq!(tree.node_end(a), 25);
    }

    #[test]
    fn test_insert_gap_before_markers() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1); // no front_advance
        tree.insert(a, 10, 20);

        tree.insert_gap(10, 5, true); // before_markers forces advance
        assert_eq!(tree.node_begin(a), 15);
        assert_eq!(tree.node_end(a), 25);
    }

    #[test]
    fn test_delete_gap_basic() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        let b = tree.alloc_node(false, false, 2);
        tree.insert(a, 10, 20);
        tree.insert(b, 30, 40);

        // Delete gap at position 15, length 5
        tree.delete_gap(15, 5);

        // 'a' has its end contracted
        assert_eq!(tree.node_begin(a), 10);
        assert_eq!(tree.node_end(a), 15);
        // 'b' shifts left
        assert_eq!(tree.node_begin(b), 25);
        assert_eq!(tree.node_end(b), 35);
    }

    #[test]
    fn test_delete_gap_containing_interval() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        tree.insert(a, 10, 20);

        // Delete gap that fully contains the interval
        tree.delete_gap(5, 20);
        assert_eq!(tree.node_begin(a), 5);
        assert_eq!(tree.node_end(a), 5);
    }

    #[test]
    fn test_iterator_ascending() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        let b = tree.alloc_node(false, false, 2);
        let c = tree.alloc_node(false, false, 3);
        tree.insert(a, 10, 20);
        tree.insert(b, 5, 15);
        tree.insert(c, 25, 35);

        // Query [0, 100) should return all three
        let mut iter = tree.iterator_start(0, 100, ItreeOrder::Ascending);
        let mut results = Vec::new();
        while let Some(n) = tree.iterator_next(&mut iter) {
            results.push(tree.node(n).data);
        }
        // Should be sorted by begin: b(5), a(10), c(25)
        assert_eq!(results, vec![2, 1, 3]);
    }

    #[test]
    fn test_iterator_descending() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        let b = tree.alloc_node(false, false, 2);
        let c = tree.alloc_node(false, false, 3);
        tree.insert(a, 10, 20);
        tree.insert(b, 5, 15);
        tree.insert(c, 25, 35);

        let mut iter = tree.iterator_start(0, 100, ItreeOrder::Descending);
        let mut results = Vec::new();
        while let Some(n) = tree.iterator_next(&mut iter) {
            results.push(tree.node(n).data);
        }
        // Should be reverse sorted by begin: c(25), a(10), b(5)
        assert_eq!(results, vec![3, 1, 2]);
    }

    #[test]
    fn test_iterator_range_query() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        let b = tree.alloc_node(false, false, 2);
        let c = tree.alloc_node(false, false, 3);
        let d = tree.alloc_node(false, false, 4);
        tree.insert(a, 1, 5);
        tree.insert(b, 10, 20);
        tree.insert(c, 15, 25);
        tree.insert(d, 30, 40);

        // Query [12, 22) should intersect b and c
        let mut iter = tree.iterator_start(12, 22, ItreeOrder::Ascending);
        let mut results = Vec::new();
        while let Some(n) = tree.iterator_next(&mut iter) {
            results.push(tree.node(n).data);
        }
        assert_eq!(results, vec![2, 3]);
    }

    #[test]
    fn test_iterator_empty_at_begin() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        tree.insert(a, 10, 10); // empty interval at position 10

        // Query exactly at the empty interval's position
        let mut iter = tree.iterator_start(10, 10, ItreeOrder::Ascending);
        let mut results = Vec::new();
        while let Some(n) = tree.iterator_next(&mut iter) {
            results.push(tree.node(n).data);
        }
        // Empty node at begin should be included
        assert_eq!(results, vec![1]);
    }

    #[test]
    fn test_gap_operations_with_many_nodes() {
        let mut tree = ItreeTree::new();
        let mut nodes = Vec::new();
        for i in 0..20 {
            let n = tree.alloc_node(i % 3 == 0, i % 2 == 0, i as u64);
            tree.insert(n, i * 10, i * 10 + 15);
            nodes.push(n);
        }
        assert!(tree.check());

        // Insert gap in the middle
        tree.insert_gap(100, 50, false);
        // Verify tree integrity
        // Can't easily check individual positions since they depend on
        // tree structure, but the tree invariants should hold
        assert_eq!(tree.size(), 20);

        // Delete some of the gap
        tree.delete_gap(100, 25);
        assert_eq!(tree.size(), 20);
    }

    #[test]
    fn test_clear() {
        let mut tree = ItreeTree::new();
        for i in 0..10 {
            let n = tree.alloc_node(false, false, i);
            tree.insert(n, i as i64, (i + 5) as i64);
        }
        assert_eq!(tree.size(), 10);

        tree.clear();
        assert!(tree.is_empty());
        assert_eq!(tree.size(), 0);
    }

    #[test]
    fn test_node_arena_reuse() {
        let mut tree = ItreeTree::new();
        let n1 = tree.alloc_node(false, false, 1);
        tree.insert(n1, 10, 20);
        tree.remove(n1);
        tree.free_node(n1);

        // Allocate again — should reuse the same slot
        let n2 = tree.alloc_node(false, false, 2);
        assert_eq!(n1, n2); // Same index
        tree.insert(n2, 30, 40);
        assert_eq!(tree.node(n2).data, 2);
        assert!(tree.check());
    }

    #[test]
    fn test_insert_remove_stress() {
        let mut tree = ItreeTree::new();
        let mut nodes = Vec::new();

        // Insert 200 nodes
        for i in 0..200 {
            let n = tree.alloc_node(i % 5 == 0, i % 3 == 0, i as u64);
            tree.insert(n, (i * 7 % 500) as i64, (i * 7 % 500 + 20) as i64);
            nodes.push(n);
        }
        assert!(tree.check());

        // Remove every other
        for i in (0..200).step_by(2) {
            tree.remove(nodes[i]);
        }
        assert_eq!(tree.size(), 100);
        assert!(tree.check());

        // Insert more
        for i in 200..300 {
            let n = tree.alloc_node(false, false, i as u64);
            tree.insert(n, (i * 11 % 600) as i64, (i * 11 % 600 + 15) as i64);
            nodes.push(n);
        }
        assert_eq!(tree.size(), 200);
        assert!(tree.check());
    }

    #[test]
    fn test_insert_gap_empty_interval_no_front_advance() {
        let mut tree = ItreeTree::new();
        // Empty interval at pos, front_advance=true but rear_advance=false
        // This means front_advance should be ignored (begin != end is false,
        // but begin == end && !rear_advance should prevent moving begin past end)
        let a = tree.alloc_node(true, false, 1);
        tree.insert(a, 10, 10);

        tree.insert_gap(10, 5, false);
        // Empty interval with front_advance but !rear_advance:
        // begin should NOT move (special case to prevent begin > end)
        assert_eq!(tree.node_begin(a), 10);
        assert_eq!(tree.node_end(a), 10);
    }

    #[test]
    fn test_iterator_narrow() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, false, 1);
        let b = tree.alloc_node(false, false, 2);
        let c = tree.alloc_node(false, false, 3);
        tree.insert(a, 5, 15);
        tree.insert(b, 10, 20);
        tree.insert(c, 25, 35);

        let mut iter = tree.iterator_start(0, 100, ItreeOrder::Ascending);
        // Narrow to [12, 30)
        iter.narrow(12, 30);

        let mut results = Vec::new();
        while let Some(n) = tree.iterator_next(&mut iter) {
            results.push(tree.node(n).data);
        }
        // a[5,15) intersects [12,30) ✓
        // b[10,20) intersects [12,30) ✓
        // c[25,35) intersects [12,30) ✓
        assert_eq!(results, vec![1, 2, 3]);
    }

    #[test]
    fn test_rear_advance() {
        let mut tree = ItreeTree::new();
        let a = tree.alloc_node(false, true, 1); // rear_advance=true
        let b = tree.alloc_node(false, false, 2); // rear_advance=false
        tree.insert(a, 10, 20);
        tree.insert(b, 10, 20);

        // Insert at end position (20)
        tree.insert_gap(20, 5, false);
        // rear_advance=true: end moves
        assert_eq!(tree.node_end(a), 25);
        // rear_advance=false: end stays
        assert_eq!(tree.node_end(b), 20);
    }
}
