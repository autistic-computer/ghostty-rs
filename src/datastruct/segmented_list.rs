//! Port of `src/datastruct/segmented_list.zig`.
//!
//! STATUS: ported
//!
//! Code taken from 0.15.2 `std.segmented_list` (MIT license). See
//! lib/compat/README.md for license and details.
//!
//! Rust notes: allocator parameters are dropped (allocation failure aborts
//! like any `Vec`), so the Zig `Allocator.Error!` results become plain
//! returns. Unlike Zig, element destructors run: shrinking/clearing drops
//! the removed elements and dropping the list drops all live elements.
//! `add_one` requires `T: Default` because Rust can't hand out references
//! to uninitialized memory, and `shrink_capacity` never releases shelves
//! that still hold live elements.

use std::mem::MaybeUninit;

use crate::quirks::inline_assert as assert;

// Imagine that `fn at(self: *Self, index: usize) &T` is a customer asking for a box
// from a warehouse, based on a flat array, boxes ordered from 0 to N - 1.
// But the warehouse actually stores boxes in shelves of increasing powers of 2 sizes.
// So when the customer requests a box index, we have to translate it to shelf index
// and box index within that shelf. Illustration:
//
// customer indexes:
// shelf 0:  0
// shelf 1:  1  2
// shelf 2:  3  4  5  6
// shelf 3:  7  8  9 10 11 12 13 14
// shelf 4: 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30
// shelf 5: 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62
// ...
//
// warehouse indexes:
// shelf 0:  0
// shelf 1:  0  1
// shelf 2:  0  1  2  3
// shelf 3:  0  1  2  3  4  5  6  7
// shelf 4:  0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
// shelf 5:  0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
// ...
//
// With this arrangement, here are the equations to get the shelf index and
// box index based on customer box index:
//
// shelf_index = floor(log2(customer_index + 1))
// shelf_count = ceil(log2(box_count + 1))
// box_index = customer_index + 1 - 2 ** shelf
// shelf_size = 2 ** shelf_index
//
// Now we complicate it a little bit further by adding a preallocated shelf, which must be
// a power of 2:
// prealloc=4
//
// customer indexes:
// prealloc:  0  1  2  3
//  shelf 0:  4  5  6  7  8  9 10 11
//  shelf 1: 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27
//  shelf 2: 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59
// ...
//
// warehouse indexes:
// prealloc:  0  1  2  3
//  shelf 0:  0  1  2  3  4  5  6  7
//  shelf 1:  0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
//  shelf 2:  0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
// ...
//
// Now the equations are:
//
// shelf_index = floor(log2(customer_index + prealloc)) - log2(prealloc) - 1
// shelf_count = ceil(log2(box_count + prealloc)) - log2(prealloc) - 1
// box_index = customer_index + prealloc - 2 ** (log2(prealloc) + 1 + shelf)
// shelf_size = prealloc * 2 ** (shelf_index + 1)

type ShelfIndex = u32;

/// A dynamically allocated shelf. The allocation never moves, which is
/// what keeps element pointers stable.
type Shelf<T> = Box<[MaybeUninit<T>]>;

/// A slot location: in the preallocated segment or in a dynamic shelf.
#[derive(Debug, Clone, Copy)]
enum Loc {
    Prealloc(usize),
    Shelf(ShelfIndex, usize),
}

/// This is a stack data structure where pointers to indexes have the same lifetime as the data structure
/// itself, unlike ArrayList where append() invalidates all existing element pointers.
/// The tradeoff is that elements are not guaranteed to be contiguous. For that, use ArrayList.
/// Note however that most elements are contiguous, making this data structure cache-friendly.
///
/// Because it never has to copy elements from an old location to a new location, it does not require
/// its elements to be copyable, and it avoids wasting memory when backed by an ArenaAllocator.
/// Note however that the append() and pop() convenience methods perform a copy, but you can instead use
/// addOne(), at(), setCapacity(), and shrinkCapacity() to avoid copying items.
///
/// This data structure has O(1) append and O(1) pop.
///
/// It supports preallocated elements, making it especially well suited when the expected maximum
/// size is small. `PREALLOC_ITEM_COUNT` must be 0, or a power of 2.
pub struct SegmentedList<T, const PREALLOC_ITEM_COUNT: usize> {
    prealloc_segment: [MaybeUninit<T>; PREALLOC_ITEM_COUNT],
    dynamic_segments: Vec<Shelf<T>>,
    pub len: usize,
}

impl<T, const P: usize> Default for SegmentedList<T, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const P: usize> Drop for SegmentedList<T, P> {
    fn drop(&mut self) {
        let len = self.len;
        self.len = 0;
        self.drop_range(0, len);
    }
}

impl<T, const P: usize> SegmentedList<T, P> {
    const PREALLOC_EXP: ShelfIndex = {
        // we don't use the prealloc_exp constant when prealloc_item_count is 0
        // but lazy-init may still be triggered by other code so supply a value
        if P == 0 {
            0
        } else {
            std::assert!(P.is_power_of_two());
            P.ilog2()
        }
    };

    pub const PREALLOC_COUNT: usize = P;

    pub const fn new() -> Self {
        Self {
            prealloc_segment: [const { MaybeUninit::uninit() }; P],
            dynamic_segments: Vec::new(),
            len: 0,
        }
    }

    /// Drop the elements in `[from, to)`. Callers have already excluded the
    /// range from `len`.
    fn drop_range(&mut self, from: usize, to: usize) {
        if !std::mem::needs_drop::<T>() {
            return;
        }
        for i in from..to {
            // SAFETY: callers only pass ranges of (formerly) live elements.
            unsafe { std::ptr::drop_in_place(self.unchecked_at_ptr_mut(i)) };
        }
    }

    pub fn at(&self, i: usize) -> &T {
        assert(i < self.len);
        // SAFETY: i < len so the slot is initialized.
        unsafe { &*self.unchecked_at_ptr(i) }
    }

    pub fn at_mut(&mut self, i: usize) -> &mut T {
        assert(i < self.len);
        // SAFETY: i < len so the slot is initialized.
        unsafe { &mut *self.unchecked_at_ptr_mut(i) }
    }

    pub fn count(&self) -> usize {
        self.len
    }

    pub fn append(&mut self, item: T) {
        let new_length = self.len + 1;
        self.grow_capacity(new_length);
        // SAFETY: capacity covers index `len`; the slot is uninitialized.
        unsafe { self.unchecked_at_ptr_mut(self.len).write(item) };
        self.len = new_length;
    }

    pub fn append_slice(&mut self, items: &[T])
    where
        T: Clone,
    {
        for item in items {
            self.append(item.clone());
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }

        let index = self.len - 1;
        // SAFETY: index < len; it is excluded from len right after.
        let result = unsafe { self.unchecked_at_ptr_mut(index).read() };
        self.len = index;
        Some(result)
    }

    pub fn add_one(&mut self) -> &mut T
    where
        T: Default,
    {
        self.append(T::default());
        let i = self.len - 1;
        self.at_mut(i)
    }

    /// Reduce length to `new_len`.
    /// Invalidates pointers for the elements at index new_len and beyond.
    pub fn shrink_retaining_capacity(&mut self, new_len: usize) {
        assert(new_len <= self.len);
        let old = self.len;
        self.len = new_len;
        self.drop_range(new_len, old);
    }

    /// Invalidates all element pointers.
    pub fn clear_retaining_capacity(&mut self) {
        self.shrink_retaining_capacity(0);
    }

    /// Invalidates all element pointers.
    pub fn clear_and_free(&mut self) {
        self.clear_retaining_capacity();
        self.set_capacity(0);
    }

    /// Grows or shrinks capacity to match usage.
    /// TODO update this and related methods to match the conventions set by ArrayList
    pub fn set_capacity(&mut self, new_capacity: usize) {
        if P != 0
            && new_capacity
                <= 1usize << (Self::PREALLOC_EXP + self.dynamic_segments.len() as ShelfIndex)
        {
            return self.shrink_capacity(new_capacity);
        }
        self.grow_capacity(new_capacity);
    }

    /// Only grows capacity, or retains current capacity.
    pub fn grow_capacity(&mut self, new_capacity: usize) {
        let new_cap_shelf_count = Self::shelf_count(new_capacity);
        let old_shelf_count = self.dynamic_segments.len() as ShelfIndex;
        if new_cap_shelf_count <= old_shelf_count {
            return;
        }

        self.dynamic_segments
            .reserve_exact((new_cap_shelf_count - old_shelf_count) as usize);
        let mut i = old_shelf_count;
        while i < new_cap_shelf_count {
            let shelf: Shelf<T> = (0..Self::shelf_size(i))
                .map(|_| MaybeUninit::uninit())
                .collect();
            self.dynamic_segments.push(shelf);
            i += 1;
        }
    }

    /// Only shrinks capacity or retains current capacity.
    /// It may fail to reduce the capacity in which case the capacity will remain unchanged.
    pub fn shrink_capacity(&mut self, new_capacity: usize) {
        // Never release storage that still holds live elements.
        let new_capacity = new_capacity.max(self.len);
        if new_capacity <= P {
            self.dynamic_segments = Vec::new();
            return;
        }

        let new_cap_shelf_count = Self::shelf_count(new_capacity);
        let old_shelf_count = self.dynamic_segments.len() as ShelfIndex;
        assert(new_cap_shelf_count <= old_shelf_count);
        if new_cap_shelf_count == old_shelf_count {
            return;
        }

        self.dynamic_segments.truncate(new_cap_shelf_count as usize);
        self.dynamic_segments.shrink_to_fit();
    }

    pub fn shrink(&mut self, new_len: usize) {
        assert(new_len <= self.len);
        // TODO take advantage of the new realloc semantics
        self.shrink_retaining_capacity(new_len);
    }

    pub fn write_to_slice(&self, dest: &mut [T], start: usize)
    where
        T: Clone,
    {
        let end = start + dest.len();
        assert(end <= self.len);

        let init = |s: &[MaybeUninit<T>]| -> &[T] {
            // SAFETY: only called on ranges below `len`.
            unsafe { &*(s as *const [MaybeUninit<T>] as *const [T]) }
        };

        let mut i = start;
        if end <= P {
            let src = init(&self.prealloc_segment[i..end]);
            dest[i - start..][..src.len()].clone_from_slice(src);
            return;
        } else if i < P {
            let src = init(&self.prealloc_segment[i..]);
            dest[i - start..][..src.len()].clone_from_slice(src);
            i = P;
        }

        while i < end {
            let shelf_index = Self::shelf_index(i);
            let copy_start = Self::box_index(i, shelf_index);
            let copy_end = Self::shelf_size(shelf_index).min(copy_start + end - i);
            let src = init(&self.dynamic_segments[shelf_index as usize][copy_start..copy_end]);
            dest[i - start..][..src.len()].clone_from_slice(src);
            i += copy_end - copy_start;
        }
    }

    /// Location of the slot for `index`.
    #[inline]
    fn loc(index: usize) -> Loc {
        if index < P {
            return Loc::Prealloc(index);
        }
        let shelf_index = Self::shelf_index(index);
        Loc::Shelf(shelf_index, Self::box_index(index, shelf_index))
    }

    #[inline]
    fn loc_ptr(&self, loc: Loc) -> *const T {
        match loc {
            Loc::Prealloc(i) => self.prealloc_segment[i].as_ptr(),
            Loc::Shelf(s, b) => self.dynamic_segments[s as usize][b].as_ptr(),
        }
    }

    #[inline]
    fn loc_ptr_mut(&mut self, loc: Loc) -> *mut T {
        match loc {
            Loc::Prealloc(i) => self.prealloc_segment[i].as_mut_ptr(),
            Loc::Shelf(s, b) => self.dynamic_segments[s as usize][b].as_mut_ptr(),
        }
    }

    /// Pointer to the slot for `index` (which must be within capacity).
    #[inline]
    fn unchecked_at_ptr(&self, index: usize) -> *const T {
        self.loc_ptr(Self::loc(index))
    }

    /// Mutable pointer to the slot for `index` (which must be within capacity).
    #[inline]
    fn unchecked_at_ptr_mut(&mut self, index: usize) -> *mut T {
        self.loc_ptr_mut(Self::loc(index))
    }

    /// Unchecked element access.
    ///
    /// # Safety
    /// `index < self.len`.
    pub unsafe fn unchecked_at(&self, index: usize) -> &T {
        &*self.unchecked_at_ptr(index)
    }

    /// Unchecked mutable element access.
    ///
    /// # Safety
    /// `index < self.len`.
    pub unsafe fn unchecked_at_mut(&mut self, index: usize) -> &mut T {
        &mut *self.unchecked_at_ptr_mut(index)
    }

    fn shelf_count(box_count: usize) -> ShelfIndex {
        if P == 0 {
            return log2_int_ceil(box_count + 1);
        }
        log2_int_ceil(box_count + P) - Self::PREALLOC_EXP - 1
    }

    fn shelf_size(shelf_index: ShelfIndex) -> usize {
        if P == 0 {
            return 1usize << shelf_index;
        }
        1usize << (shelf_index + (Self::PREALLOC_EXP + 1))
    }

    fn shelf_index(list_index: usize) -> ShelfIndex {
        if P == 0 {
            return (list_index + 1).ilog2();
        }
        (list_index + P).ilog2() - Self::PREALLOC_EXP - 1
    }

    fn box_index(list_index: usize, shelf_index: ShelfIndex) -> usize {
        if P == 0 {
            return (list_index + 1) - (1usize << shelf_index);
        }
        list_index + P - (1usize << ((Self::PREALLOC_EXP + 1) + shelf_index))
    }

    pub fn iterator(&mut self, start_index: usize) -> Iterator<'_, T, P> {
        let mut it = Iterator {
            base: BaseIterator::new(),
            list: self,
        };
        it.set(start_index);
        it
    }

    pub fn const_iterator(&self, start_index: usize) -> ConstIterator<'_, T, P> {
        let mut it = ConstIterator {
            base: BaseIterator::new(),
            list: self,
        };
        it.set(start_index);
        it
    }
}

/// Shared iterator state (Zig `BaseIterator`). It computes slot locations;
/// the typed iterators turn them into references.
#[derive(Debug, Clone, Copy)]
struct BaseIterator {
    index: usize,
    box_index: usize,
    shelf_index: ShelfIndex,
    shelf_size: usize,
}

impl BaseIterator {
    fn new() -> Self {
        Self {
            index: 0,
            box_index: 0,
            shelf_index: 0,
            shelf_size: 0,
        }
    }

    fn next<const P: usize>(&mut self, len: usize) -> Option<Loc> {
        if self.index >= len {
            return None;
        }
        if self.index < P {
            let loc = Loc::Prealloc(self.index);
            self.index += 1;
            if self.index == P {
                self.box_index = 0;
                self.shelf_index = 0;
                self.shelf_size = P * 2;
            }
            return Some(loc);
        }

        let loc = Loc::Shelf(self.shelf_index, self.box_index);
        self.index += 1;
        self.box_index += 1;
        if self.box_index == self.shelf_size {
            self.shelf_index += 1;
            self.box_index = 0;
            self.shelf_size *= 2;
        }
        Some(loc)
    }

    fn prev<const P: usize>(&mut self) -> Option<Loc> {
        if self.index == 0 {
            return None;
        }

        self.index -= 1;
        if self.index < P {
            return Some(Loc::Prealloc(self.index));
        }

        if self.box_index == 0 {
            self.shelf_index -= 1;
            self.shelf_size /= 2;
            self.box_index = self.shelf_size - 1;
        } else {
            self.box_index -= 1;
        }

        Some(Loc::Shelf(self.shelf_index, self.box_index))
    }

    fn peek<const P: usize>(&self, len: usize) -> Option<Loc> {
        if self.index >= len {
            return None;
        }
        if self.index < P {
            return Some(Loc::Prealloc(self.index));
        }

        Some(Loc::Shelf(self.shelf_index, self.box_index))
    }

    fn set<T, const P: usize>(&mut self, index: usize) {
        self.index = index;
        if index < P {
            return;
        }
        self.shelf_index = SegmentedList::<T, P>::shelf_index(index);
        self.box_index = SegmentedList::<T, P>::box_index(index, self.shelf_index);
        self.shelf_size = SegmentedList::<T, P>::shelf_size(self.shelf_index);
    }
}

/// Mutable iterator. `next`/`prev`/`peek` return references borrowed from
/// the iterator.
pub struct Iterator<'a, T, const P: usize> {
    list: &'a mut SegmentedList<T, P>,
    base: BaseIterator,
}

impl<T, const P: usize> Iterator<'_, T, P> {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&mut T> {
        let loc = self.base.next::<P>(self.list.len)?;
        // SAFETY: the iterator only yields slots below `len`.
        Some(unsafe { &mut *self.list.loc_ptr_mut(loc) })
    }

    pub fn prev(&mut self) -> Option<&mut T> {
        let loc = self.base.prev::<P>()?;
        // SAFETY: the iterator only yields slots below `len`.
        Some(unsafe { &mut *self.list.loc_ptr_mut(loc) })
    }

    pub fn peek(&mut self) -> Option<&mut T> {
        let loc = self.base.peek::<P>(self.list.len)?;
        // SAFETY: the iterator only yields slots below `len`.
        Some(unsafe { &mut *self.list.loc_ptr_mut(loc) })
    }

    pub fn set(&mut self, index: usize) {
        self.base.set::<T, P>(index);
    }
}

/// Shared-reference iterator.
pub struct ConstIterator<'a, T, const P: usize> {
    list: &'a SegmentedList<T, P>,
    base: BaseIterator,
}

impl<'a, T, const P: usize> ConstIterator<'a, T, P> {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&'a T> {
        let loc = self.base.next::<P>(self.list.len)?;
        // SAFETY: the iterator only yields slots below `len`.
        Some(unsafe { &*self.list.loc_ptr(loc) })
    }

    pub fn prev(&mut self) -> Option<&'a T> {
        let loc = self.base.prev::<P>()?;
        // SAFETY: the iterator only yields slots below `len`.
        Some(unsafe { &*self.list.loc_ptr(loc) })
    }

    pub fn peek(&self) -> Option<&'a T> {
        let loc = self.base.peek::<P>(self.list.len)?;
        // SAFETY: the iterator only yields slots below `len`.
        Some(unsafe { &*self.list.loc_ptr(loc) })
    }

    pub fn set(&mut self, index: usize) {
        self.base.set::<T, P>(index);
    }
}

/// TODO look into why this std.math function was changed in
/// fc9430f56798a53f9393a697f4ccd6bf9981b970.
fn log2_int_ceil(x: usize) -> ShelfIndex {
    assert(x != 0);
    let log2_val = x.ilog2();
    if 1usize << log2_val == x {
        return log2_val;
    }
    log2_val + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_usage() {
        test_segmented_list::<0>();
        test_segmented_list::<1>();
        test_segmented_list::<2>();
        test_segmented_list::<4>();
        test_segmented_list::<8>();
        test_segmented_list::<16>();
    }

    fn test_segmented_list<const PREALLOC: usize>() {
        let mut list = SegmentedList::<i32, PREALLOC>::new();

        {
            let mut i: usize = 0;
            while i < 100 {
                list.append((i + 1) as i32);
                assert_eq!(list.len, i + 1);
                i += 1;
            }
        }

        {
            let mut i: usize = 0;
            while i < 100 {
                assert_eq!(*list.at(i), (i + 1) as i32);
                i += 1;
            }
        }

        {
            let mut it = list.iterator(0);
            let mut x: i32 = 0;
            while let Some(item) = it.next() {
                x += 1;
                assert_eq!(*item, x);
            }
            assert_eq!(x, 100);
            while let Some(item) = it.prev() {
                assert_eq!(*item, x);
                x -= 1;
            }
            assert_eq!(x, 0);
        }

        {
            let mut it = list.const_iterator(0);
            let mut x: i32 = 0;
            while let Some(item) = it.next() {
                x += 1;
                assert_eq!(*item, x);
            }
            assert_eq!(x, 100);
            while let Some(item) = it.prev() {
                assert_eq!(*item, x);
                x -= 1;
            }
            assert_eq!(x, 0);
        }

        assert_eq!(list.pop().unwrap(), 100);
        assert_eq!(list.len, 99);

        list.append_slice(&[1, 2, 3]);
        assert_eq!(list.len, 102);
        assert_eq!(list.pop().unwrap(), 3);
        assert_eq!(list.pop().unwrap(), 2);
        assert_eq!(list.pop().unwrap(), 1);
        assert_eq!(list.len, 99);

        list.append_slice(&[]);
        assert_eq!(list.len, 99);

        {
            let mut i: i32 = 99;
            while let Some(item) = list.pop() {
                assert_eq!(item, i);
                let len = list.len;
                list.shrink_capacity(len);
                i -= 1;
            }
        }

        {
            let mut control = [0i32; 100];
            let mut dest = [0i32; 100];

            let mut i: i32 = 0;
            while i < 100 {
                list.append(i + 1);
                control[i as usize] = i + 1;
                i += 1;
            }

            dest.fill(0);
            list.write_to_slice(&mut dest[..], 0);
            assert_eq!(control[..], dest[..]);

            dest.fill(0);
            list.write_to_slice(&mut dest[50..], 50);
            assert_eq!(control[50..], dest[50..]);
        }

        list.set_capacity(0);
    }

    #[test]
    fn clear_retaining_capacity() {
        let mut list = SegmentedList::<i32, 1>::new();

        list.append_slice(&[4, 5]);
        list.clear_retaining_capacity();
        list.append(6);
        assert_eq!(*list.at(0), 6);
        assert_eq!(list.len, 1);
        list.clear_retaining_capacity();
        assert_eq!(list.len, 0);
    }

    #[test]
    fn drops_elements() {
        // Not in the Zig file: element destructors run in Rust.
        use std::rc::Rc;
        let rc = Rc::new(());
        {
            let mut list = SegmentedList::<Rc<()>, 2>::new();
            for _ in 0..10 {
                list.append(rc.clone());
            }
            assert_eq!(Rc::strong_count(&rc), 11);
            list.shrink_retaining_capacity(5);
            assert_eq!(Rc::strong_count(&rc), 6);
            drop(list.pop());
            assert_eq!(Rc::strong_count(&rc), 5);
        }
        assert_eq!(Rc::strong_count(&rc), 1);
    }
}
