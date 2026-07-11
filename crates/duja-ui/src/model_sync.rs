//! In-place synchronisation of a Slint list model.
//!
//! A `for row in model` repeater tears down and recreates **all** of its child
//! elements whenever the backing model is *reset* — which is exactly what
//! [`VecModel::set_vec`](slint::VecModel::set_vec) does (it fires
//! `ModelNotify::reset`). Recreating the elements destroys any in-progress
//! pointer/touch grab living on a child widget, so a slider drag dies the moment
//! an unrelated re-render replaces the model (P0 live-QA bug 3).
//!
//! [`sync`] instead diffs the model **in place**: it updates only the rows whose
//! data changed (`set_row_data` ⇒ `ModelNotify::row_changed`, which pushes new
//! data into the *existing* element without recreating it) and grows/shrinks the
//! tail with `push`/`remove`. An active drag on an untouched row therefore
//! survives a fan-out re-render of the other rows.
//!
//! The logic is written against the tiny [`RowModel`] seam so it is unit-tested
//! without a Slint backend; the production impl is the blanket one for
//! [`VecModel`].

use slint::{Model, VecModel};

/// The minimal list-model surface [`sync`] needs: count, read, and the three
/// **non-resetting** mutations. Deliberately has no "reset/replace" operation —
/// that is the whole point (a reset is what recreates the repeater's elements).
pub(crate) trait RowModel<T> {
    /// The current row count.
    fn count(&self) -> usize;
    /// The data at `index`, if in range.
    fn get(&self, index: usize) -> Option<T>;
    /// Replace the data at `index` in place (existing element kept).
    fn set(&self, index: usize, value: T);
    /// Append a new row (a new element is created only for the appended tail).
    fn push(&self, value: T);
    /// Remove the row at `index`.
    fn remove(&self, index: usize);
}

impl<T: Clone + 'static> RowModel<T> for VecModel<T> {
    fn count(&self) -> usize {
        self.row_count()
    }
    fn get(&self, index: usize) -> Option<T> {
        self.row_data(index)
    }
    fn set(&self, index: usize, value: T) {
        self.set_row_data(index, value);
    }
    fn push(&self, value: T) {
        VecModel::push(self, value);
    }
    fn remove(&self, index: usize) {
        VecModel::remove(self, index);
    }
}

/// Update `model` in place so its rows equal `new`, without ever resetting it.
///
/// Unchanged rows are left completely untouched (no `row_changed`), so a row a
/// user is actively dragging is never disturbed by a re-render triggered from a
/// *different* row. Changed rows are updated with `set` (in place), and the tail
/// is grown/shrunk with `push`/`remove`.
pub(crate) fn sync<T, M>(model: &M, new: Vec<T>)
where
    T: Clone + PartialEq,
    M: RowModel<T> + ?Sized,
{
    let old_len = model.count();
    let new_len = new.len();
    for (index, item) in new.into_iter().enumerate() {
        if index < old_len {
            // Only rewrite rows that actually changed: this both avoids needless
            // element updates and guarantees an in-drag row whose value is
            // unchanged is never written over.
            if model.get(index).as_ref() != Some(&item) {
                model.set(index, item);
            }
        } else {
            model.push(item);
        }
    }
    // Trim any surplus rows from the end (highest index first).
    for index in (new_len..old_len).rev() {
        model.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A recorded mutation, so a test can assert the model was diffed in place
    /// rather than cleared and rebuilt.
    #[derive(Debug, PartialEq, Eq)]
    enum Op {
        Set(usize),
        Push,
        Remove(usize),
    }

    /// A backend-free spy implementing [`RowModel`].
    struct Spy {
        rows: RefCell<Vec<i32>>,
        ops: RefCell<Vec<Op>>,
    }

    impl Spy {
        fn new(rows: Vec<i32>) -> Self {
            Spy {
                rows: RefCell::new(rows),
                ops: RefCell::new(Vec::new()),
            }
        }
        fn rows(&self) -> Vec<i32> {
            self.rows.borrow().clone()
        }
        fn ops(&self) -> Vec<Op> {
            std::mem::take(&mut self.ops.borrow_mut())
        }
    }

    impl RowModel<i32> for Spy {
        fn count(&self) -> usize {
            self.rows.borrow().len()
        }
        fn get(&self, index: usize) -> Option<i32> {
            self.rows.borrow().get(index).copied()
        }
        fn set(&self, index: usize, value: i32) {
            if let Some(slot) = self.rows.borrow_mut().get_mut(index) {
                *slot = value;
            }
            self.ops.borrow_mut().push(Op::Set(index));
        }
        fn push(&self, value: i32) {
            self.rows.borrow_mut().push(value);
            self.ops.borrow_mut().push(Op::Push);
        }
        fn remove(&self, index: usize) {
            self.rows.borrow_mut().remove(index);
            self.ops.borrow_mut().push(Op::Remove(index));
        }
    }

    #[test]
    fn changed_row_is_updated_in_place_and_others_untouched() {
        let spy = Spy::new(vec![10, 20, 30]);
        sync(&spy, vec![10, 99, 30]);
        // Exactly one in-place set at the changed index; no reset, no churn on
        // the unchanged rows (this is what keeps an in-drag row's grab alive).
        assert_eq!(spy.ops(), vec![Op::Set(1)]);
        assert_eq!(spy.rows(), vec![10, 99, 30]);
    }

    #[test]
    fn identical_update_touches_nothing() {
        let spy = Spy::new(vec![1, 2, 3]);
        sync(&spy, vec![1, 2, 3]);
        assert!(spy.ops().is_empty());
    }

    #[test]
    fn growth_pushes_only_the_new_tail() {
        let spy = Spy::new(vec![1]);
        sync(&spy, vec![1, 2, 3]);
        assert_eq!(spy.ops(), vec![Op::Push, Op::Push]);
        assert_eq!(spy.rows(), vec![1, 2, 3]);
    }

    #[test]
    fn shrink_removes_surplus_tail_from_the_end() {
        let spy = Spy::new(vec![1, 2, 3, 4]);
        sync(&spy, vec![1, 2]);
        assert_eq!(spy.ops(), vec![Op::Remove(3), Op::Remove(2)]);
        assert_eq!(spy.rows(), vec![1, 2]);
    }

    #[test]
    fn combined_change_and_shrink() {
        let spy = Spy::new(vec![1, 2, 3]);
        sync(&spy, vec![1, 9]);
        assert_eq!(spy.ops(), vec![Op::Set(1), Op::Remove(2)]);
        assert_eq!(spy.rows(), vec![1, 9]);
    }
}
