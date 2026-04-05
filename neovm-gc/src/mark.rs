//! Mark-phase worklist primitives.

/// Splittable LIFO worklist used by mark tracers.
#[derive(Debug, Default)]
pub(crate) struct MarkWorklist<T> {
    entries: Vec<T>,
}

impl<T> MarkWorklist<T> {
    pub(crate) fn push(&mut self, value: T) {
        self.entries.push(value);
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        self.entries.pop()
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn split_half(&mut self) -> Self {
        let split_at = self.entries.len() / 2;
        let entries = self.entries.split_off(split_at);
        Self { entries }
    }

    pub(crate) fn append(&mut self, other: &mut Self) {
        self.entries.append(&mut other.entries);
    }
}

#[cfg(test)]
mod tests {
    use super::MarkWorklist;

    #[test]
    fn mark_worklist_is_lifo() {
        let mut worklist = MarkWorklist::default();
        worklist.push(1usize);
        worklist.push(2usize);
        worklist.push(3usize);

        assert_eq!(worklist.pop(), Some(3));
        assert_eq!(worklist.pop(), Some(2));
        assert_eq!(worklist.pop(), Some(1));
        assert_eq!(worklist.pop(), None);
        assert!(worklist.is_empty());
    }

    #[test]
    fn mark_worklist_split_half_moves_upper_slice() {
        let mut worklist = MarkWorklist::default();
        for value in 0..6usize {
            worklist.push(value);
        }

        let mut stolen = worklist.split_half();
        assert_eq!(worklist.len(), 3);
        assert_eq!(stolen.len(), 3);
        assert_eq!(stolen.pop(), Some(5));
        assert_eq!(stolen.pop(), Some(4));
        assert_eq!(stolen.pop(), Some(3));
        assert_eq!(worklist.pop(), Some(2));
        assert_eq!(worklist.pop(), Some(1));
        assert_eq!(worklist.pop(), Some(0));
    }

    #[test]
    fn mark_worklist_append_preserves_lifo_tail() {
        let mut left = MarkWorklist::default();
        left.push(1usize);
        left.push(2usize);

        let mut right = MarkWorklist::default();
        right.push(3usize);
        right.push(4usize);

        left.append(&mut right);
        assert_eq!(right.len(), 0);
        assert_eq!(left.pop(), Some(4));
        assert_eq!(left.pop(), Some(3));
        assert_eq!(left.pop(), Some(2));
        assert_eq!(left.pop(), Some(1));
    }
}
