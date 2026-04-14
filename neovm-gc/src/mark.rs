//! Mark-phase worklist primitives.

/// Splittable LIFO worklist used by mark tracers.
#[derive(Debug)]
pub(crate) struct MarkWorklist<T> {
    entries: Vec<T>,
}

impl<T> Default for MarkWorklist<T> {
    fn default() -> Self {
        Self {
            entries: Vec::default(),
        }
    }
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
#[path = "mark_test.rs"]
mod tests;
