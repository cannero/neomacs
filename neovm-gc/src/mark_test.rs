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
