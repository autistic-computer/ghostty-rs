//! Port of `src/datastruct/intrusive_linked_list.zig`.
//!
//! STATUS: ported
//!
//! The list stores raw (`NonNull`) pointers to nodes it does not own, just
//! like the Zig version. All mutating operations are therefore `unsafe`:
//! callers guarantee that every node pointer is valid, that nodes outlive
//! their membership in the list, and that a node is in at most one list
//! (or the right one) at a time.

use std::ptr::NonNull;

/// A node type usable with [`DoublyLinkedList`]. The type must have a
/// "next" and "prev" link pointing to itself. Use
/// [`impl_intrusive_node!`](crate::impl_intrusive_node) for the common case
/// of `next`/`prev` fields of type `Option<NonNull<Self>>`.
pub trait Node: Sized {
    fn next(&self) -> Option<NonNull<Self>>;
    fn prev(&self) -> Option<NonNull<Self>>;
    fn set_next(&mut self, next: Option<NonNull<Self>>);
    fn set_prev(&mut self, prev: Option<NonNull<Self>>);
}

/// Implement [`Node`] for a struct with `next` and `prev` fields of type
/// `Option<NonNull<Self>>`.
#[macro_export]
macro_rules! impl_intrusive_node {
    ($t:ty) => {
        impl $crate::datastruct::intrusive_linked_list::Node for $t {
            #[inline(always)]
            fn next(&self) -> Option<::std::ptr::NonNull<Self>> {
                self.next
            }
            #[inline(always)]
            fn prev(&self) -> Option<::std::ptr::NonNull<Self>> {
                self.prev
            }
            #[inline(always)]
            fn set_next(&mut self, next: Option<::std::ptr::NonNull<Self>>) {
                self.next = next;
            }
            #[inline(always)]
            fn set_prev(&mut self, prev: Option<::std::ptr::NonNull<Self>>) {
                self.prev = prev;
            }
        }
    };
}

/// An intrusive doubly-linked list. The type T must have a "next" and "prev"
/// field pointing to itself.
///
/// This is an adaptation of the DoublyLinkedList from the Zig standard
/// library, which is MIT licensed. I've removed functionality that I don't
/// need.
#[derive(Debug)]
pub struct DoublyLinkedList<T> {
    pub first: Option<NonNull<T>>,
    pub last: Option<NonNull<T>>,
}

impl<T> Default for DoublyLinkedList<T> {
    fn default() -> Self {
        Self {
            first: None,
            last: None,
        }
    }
}

// The list itself is just two pointers; copying it is how Zig code passes
// it around by value.
impl<T> Clone for DoublyLinkedList<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for DoublyLinkedList<T> {}

impl<T: Node> DoublyLinkedList<T> {
    pub const fn new() -> Self {
        Self {
            first: None,
            last: None,
        }
    }

    /// Insert a new node after an existing one.
    ///
    /// Arguments:
    ///     node: Pointer to a node in the list.
    ///     new_node: Pointer to the new node to insert.
    ///
    /// # Safety
    /// Both pointers must be valid; `node` must be in this list and
    /// `new_node` must not be in any list.
    #[inline(always)]
    pub unsafe fn insert_after(&mut self, node: NonNull<T>, mut new_node: NonNull<T>) {
        let node_ref = &mut *node.as_ptr();
        new_node.as_mut().set_prev(Some(node));
        if let Some(mut next_node) = node_ref.next() {
            // Intermediate node.
            new_node.as_mut().set_next(Some(next_node));
            next_node.as_mut().set_prev(Some(new_node));
        } else {
            // Last element of the list.
            new_node.as_mut().set_next(None);
            self.last = Some(new_node);
        }
        node_ref.set_next(Some(new_node));
    }

    /// Insert a new node before an existing one.
    ///
    /// Arguments:
    ///     node: Pointer to a node in the list.
    ///     new_node: Pointer to the new node to insert.
    ///
    /// # Safety
    /// See [`DoublyLinkedList::insert_after`].
    #[inline(always)]
    pub unsafe fn insert_before(&mut self, node: NonNull<T>, mut new_node: NonNull<T>) {
        let node_ref = &mut *node.as_ptr();
        new_node.as_mut().set_next(Some(node));
        if let Some(mut prev_node) = node_ref.prev() {
            // Intermediate node.
            new_node.as_mut().set_prev(Some(prev_node));
            prev_node.as_mut().set_next(Some(new_node));
        } else {
            // First element of the list.
            new_node.as_mut().set_prev(None);
            self.first = Some(new_node);
        }
        node_ref.set_prev(Some(new_node));
    }

    /// Insert a new node at the end of the list.
    ///
    /// Arguments:
    ///     new_node: Pointer to the new node to insert.
    ///
    /// # Safety
    /// `new_node` must be valid and not in any list.
    #[inline(always)]
    pub unsafe fn append(&mut self, new_node: NonNull<T>) {
        if let Some(last) = self.last {
            // Insert after last.
            self.insert_after(last, new_node);
        } else {
            // Empty list.
            self.prepend(new_node);
        }
    }

    /// Insert a new node at the beginning of the list.
    ///
    /// Arguments:
    ///     new_node: Pointer to the new node to insert.
    ///
    /// # Safety
    /// `new_node` must be valid and not in any list.
    #[inline(always)]
    pub unsafe fn prepend(&mut self, mut new_node: NonNull<T>) {
        if let Some(first) = self.first {
            // Insert before first.
            self.insert_before(first, new_node);
        } else {
            // Empty list.
            self.first = Some(new_node);
            self.last = Some(new_node);
            new_node.as_mut().set_prev(None);
            new_node.as_mut().set_next(None);
        }
    }

    /// Remove a node from the list.
    ///
    /// Arguments:
    ///     node: Pointer to the node to be removed.
    ///
    /// # Safety
    /// `node` must be valid and in this list.
    #[inline(always)]
    pub unsafe fn remove(&mut self, node: NonNull<T>) {
        let node_ref = &*node.as_ptr();
        let next = node_ref.next();
        let prev = node_ref.prev();
        if let Some(mut prev_node) = prev {
            // Intermediate node.
            prev_node.as_mut().set_next(next);
        } else {
            // First element of the list.
            self.first = next;
        }

        if let Some(mut next_node) = next {
            // Intermediate node.
            next_node.as_mut().set_prev(prev);
        } else {
            // Last element of the list.
            self.last = prev;
        }
    }

    /// Remove and return the last node in the list.
    ///
    /// Returns:
    ///     A pointer to the last node in the list.
    ///
    /// # Safety
    /// All nodes in the list must be valid.
    #[inline(always)]
    pub unsafe fn pop(&mut self) -> Option<NonNull<T>> {
        let last = self.last?;
        self.remove(last);
        Some(last)
    }

    /// Remove and return the first node in the list.
    ///
    /// Returns:
    ///     A pointer to the first node in the list.
    ///
    /// # Safety
    /// All nodes in the list must be valid.
    #[inline(always)]
    pub unsafe fn pop_first(&mut self) -> Option<NonNull<T>> {
        let first = self.first?;
        self.remove(first);
        Some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestNode {
        data: u32,
        prev: Option<NonNull<TestNode>>,
        next: Option<NonNull<TestNode>>,
    }
    crate::impl_intrusive_node!(TestNode);

    impl TestNode {
        fn new(data: u32) -> Self {
            Self {
                data,
                prev: None,
                next: None,
            }
        }
    }

    #[test]
    fn basic_doubly_linked_list_test() {
        type L = DoublyLinkedList<TestNode>;
        let mut list: L = L::new();

        let mut one = TestNode::new(1);
        let mut two = TestNode::new(2);
        let mut three = TestNode::new(3);
        let mut four = TestNode::new(4);
        let mut five = TestNode::new(5);
        let one_p = NonNull::from(&mut one);
        let two_p = NonNull::from(&mut two);
        let three_p = NonNull::from(&mut three);
        let four_p = NonNull::from(&mut four);
        let five_p = NonNull::from(&mut five);

        unsafe {
            list.append(two_p); // {2}
            list.append(five_p); // {2, 5}
            list.prepend(one_p); // {1, 2, 5}
            list.insert_before(five_p, four_p); // {1, 2, 4, 5}
            list.insert_after(two_p, three_p); // {1, 2, 3, 4, 5}

            // Traverse forwards.
            {
                let mut it = list.first;
                let mut index: u32 = 1;
                while let Some(node) = it {
                    assert_eq!(node.as_ref().data, index);
                    index += 1;
                    it = node.as_ref().next;
                }
            }

            // Traverse backwards.
            {
                let mut it = list.last;
                let mut index: u32 = 1;
                while let Some(node) = it {
                    assert_eq!(node.as_ref().data, 6 - index);
                    index += 1;
                    it = node.as_ref().prev;
                }
            }

            let _ = list.pop_first(); // {2, 3, 4, 5}
            let _ = list.pop(); // {2, 3, 4}
            list.remove(three_p); // {2, 4}

            assert_eq!(list.first.unwrap().as_ref().data, 2);
            assert_eq!(list.last.unwrap().as_ref().data, 4);
        }
    }
}
