//! Port of `src/datastruct/circ_buf.zig`.
//!
//! STATUS: ported
//!
//! The Zig version takes the default value as a comptime parameter
//! (`CircBuf(T, default)`). Rust const generics can't carry arbitrary
//! values, so the default is passed to [`CircBuf::init`] and stored.
//! Zig callers that use `undefined` as the default can pass any value
//! (e.g. `T::default()`).

use std::fmt;

use crate::fastmem;
use crate::quirks::inline_assert as assert;

/// Errors returned by [`CircBuf`] (Zig: `Allocator.Error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OutOfMemory => f.write_str("OutOfMemory"),
        }
    }
}

impl std::error::Error for Error {}

/// Iteration direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Reverse,
}

/// Returns a circular buffer containing type T.
#[derive(Debug, Clone)]
pub struct CircBuf<T> {
    // Implementation note: there's a lot of unsafe addition of usize
    // here in this implementation that can technically overflow. If someone
    // wants to fix this and make it overflow safe (use subtractions for
    // checks prior to additions) then I welcome it. In reality, we'd
    // have to be a really, really large terminal screen to even worry
    // about this so I'm punting it.
    pub storage: Vec<T>,
    pub head: usize,
    pub tail: usize,

    // We could remove this and just use math with head/tail to figure
    // it out, but our usage of circular buffers stores so much data that
    // this minor overhead is not worth optimizing out.
    pub full: bool,

    /// The default value new/cleared slots are set to.
    default: T,
}

/// Iterator over a [`CircBuf`].
pub struct Iterator<'a, T> {
    buf: &'a CircBuf<T>,
    idx: usize,
    direction: Direction,
}

impl<'a, T: Clone> Iterator<'a, T> {
    #[inline]
    fn storage_idx(&self) -> Option<usize> {
        storage_index(self.buf, self.idx, self.direction)
    }

    /// Seek the iterator by a given amount. This will clamp
    /// the values to the bounds of the buffer so overflows are
    /// not possible.
    pub fn seek_by(&mut self, amount: isize) {
        seek(&mut self.idx, amount);
    }

    /// Reset the iterator back to the first value.
    pub fn reset(&mut self) {
        self.idx = 0;
    }
}

impl<'a, T: Clone> std::iter::Iterator for Iterator<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        let storage_idx = self.storage_idx()?;
        self.idx += 1;
        Some(&self.buf.storage[storage_idx])
    }
}

/// Mutable iterator over a [`CircBuf`]. `next` hands out a mutable
/// reference borrowed from the iterator (the Zig iterator returns `*T`).
pub struct IteratorMut<'a, T> {
    buf: &'a mut CircBuf<T>,
    idx: usize,
    direction: Direction,
}

impl<T: Clone> IteratorMut<'_, T> {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&mut T> {
        let storage_idx = storage_index(self.buf, self.idx, self.direction)?;
        self.idx += 1;
        Some(&mut self.buf.storage[storage_idx])
    }

    /// See [`Iterator::seek_by`].
    pub fn seek_by(&mut self, amount: isize) {
        seek(&mut self.idx, amount);
    }

    /// Reset the iterator back to the first value.
    pub fn reset(&mut self) {
        self.idx = 0;
    }
}

#[inline]
fn storage_index<T: Clone>(buf: &CircBuf<T>, idx: usize, direction: Direction) -> Option<usize> {
    if idx >= buf.len() {
        return None;
    }

    // Get our index from the tail
    let tail_idx = match direction {
        Direction::Forward => idx,
        Direction::Reverse => buf.len() - idx - 1,
    };

    // Translate the tail index to a storage index
    Some((buf.tail + tail_idx) % buf.capacity())
}

#[inline]
fn seek(idx: &mut usize, amount: isize) {
    if amount > 0 {
        *idx = idx.saturating_add(amount as usize);
    } else {
        *idx = idx.saturating_sub(amount.unsigned_abs());
    }
}

impl<T: Clone> CircBuf<T> {
    /// Initialize a new circular buffer that can store size elements.
    pub fn init(size: usize, default: T) -> Self {
        Self {
            storage: vec![default.clone(); size],
            head: 0,
            tail: 0,
            full: size == 0,
            default,
        }
    }

    /// Append a single value to the buffer. If the buffer is full,
    /// an error will be returned.
    pub fn append(&mut self, v: T) -> Result<(), Error> {
        if self.full {
            return Err(Error::OutOfMemory);
        }
        self.storage[self.head] = v;
        self.head += 1;
        if self.head >= self.storage.len() {
            self.head = 0;
        }
        self.full = self.head == self.tail;
        Ok(())
    }

    /// Append a single value to the buffer, assuming there is capacity.
    pub fn append_assume_capacity(&mut self, v: T) {
        assert(!self.full);
        self.storage[self.head] = v;
        self.head += 1;
        if self.head >= self.storage.len() {
            self.head = 0;
        }
        self.full = self.head == self.tail;
    }

    /// Append a slice to the buffer.
    pub fn append_slice_assume_capacity(&mut self, slice: &[T]) {
        let len = self.len();
        let [a, b] = self.get_ptr_slice(len, slice.len());
        let a_len = a.len();
        a.clone_from_slice(&slice[..a_len]);
        b.clone_from_slice(&slice[a_len..]);
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.full = false;
    }

    /// Iterate over the circular buffer.
    pub fn iterator(&self, direction: Direction) -> Iterator<'_, T> {
        Iterator {
            buf: self,
            idx: 0,
            direction,
        }
    }

    /// Iterate over the circular buffer, yielding mutable references.
    pub fn iterator_mut(&mut self, direction: Direction) -> IteratorMut<'_, T> {
        IteratorMut {
            buf: self,
            idx: 0,
            direction,
        }
    }

    /// Get the first (oldest) value in the buffer.
    pub fn first(&self) -> Option<&T> {
        // Note: this can be more efficient by not using the
        // iterator, but this was an easy way to implement it.
        let mut it = self.iterator(Direction::Forward);
        it.next()
    }

    /// Get the last (newest) value in the buffer.
    pub fn last(&self) -> Option<&T> {
        // Note: this can be more efficient by not using the
        // iterator, but this was an easy way to implement it.
        let mut it = self.iterator(Direction::Reverse);
        it.next()
    }

    /// Mutable variant of [`CircBuf::first`].
    pub fn first_mut(&mut self) -> Option<&mut T> {
        let idx = storage_index(self, 0, Direction::Forward)?;
        Some(&mut self.storage[idx])
    }

    /// Mutable variant of [`CircBuf::last`].
    pub fn last_mut(&mut self) -> Option<&mut T> {
        let idx = storage_index(self, 0, Direction::Reverse)?;
        Some(&mut self.storage[idx])
    }

    /// Ensures that there is enough capacity to store amount more
    /// items via append.
    pub fn ensure_unused_capacity(&mut self, amount: usize) {
        let new_cap = self.len() + amount;
        if new_cap <= self.capacity() {
            return;
        }
        self.resize(new_cap);
    }

    /// Resize the buffer to the given size (larger or smaller).
    /// If larger, new values will be set to the default value.
    pub fn resize(&mut self, size: usize) {
        // Rotate to zero so it is aligned.
        self.rotate_to_zero();

        // Reallocate, this adds to the end so we're ready to go.
        // If we grew, we need to set our new defaults. We can add it
        // at the end since we rotated to start.
        let prev_len = self.len();
        self.storage.resize(size, self.default.clone());
        let retained_len = prev_len.min(size);

        // Fix up our head/tail for the new capacity.
        self.head = if retained_len == size {
            0
        } else {
            retained_len
        };
        self.tail = 0;
        self.full = retained_len == size;
    }

    /// Rotate the data so that it is zero-aligned.
    fn rotate_to_zero(&mut self) {
        // If we're already at zero then do nothing.
        if self.tail == 0 {
            return;
        }

        // We use std.mem.rotate to rotate our storage in-place.
        self.storage.rotate_left(self.tail);

        // Then fix up our head and tail.
        self.head = self.len() % self.storage.len();
        self.tail = 0;
    }

    /// Returns if the buffer is currently empty. To check if its
    /// full, just check the "full" attribute.
    pub fn empty(&self) -> bool {
        !self.full && self.head == self.tail
    }

    /// Returns the total capacity allocated for this buffer.
    pub fn capacity(&self) -> usize {
        self.storage.len()
    }

    /// Returns the length in elements that are used.
    pub fn len(&self) -> usize {
        if self.full {
            return self.storage.len();
        }
        if self.head >= self.tail {
            return self.head - self.tail;
        }
        self.storage.len() - (self.tail - self.head)
    }

    /// Alias of [`CircBuf::empty`] for Rust conventions.
    pub fn is_empty(&self) -> bool {
        self.empty()
    }

    /// Delete the oldest n values from the buffer. If there are less
    /// than n values in the buffer, it'll delete everything.
    pub fn delete_oldest(&mut self, n: usize) {
        assert(n <= self.storage.len());

        // Special case n == 0 otherwise we will accidentally break
        // our circular buffer.
        if n == 0 {
            return;
        }

        // Clear the values back to default
        let default = self.default.clone();
        for slice in self.get_ptr_slice(0, n) {
            slice.fill(default.clone());
        }

        // If we're not full, we can just advance the tail. We know
        // it'll be less than the length because otherwise we'd be full.
        self.tail += self.len().min(n);
        if self.tail >= self.storage.len() {
            self.tail -= self.storage.len();
        }
        self.full = false;
    }

    /// Returns a pointer to the value at offset with the given length,
    /// and considers this full amount of data "written" if it is beyond
    /// the end of our buffer. This never "rotates" the buffer because
    /// the offset can only be within the size of the buffer.
    pub fn get_ptr_slice(&mut self, offset: usize, slice_len: usize) -> [&mut [T]; 2] {
        // Special case the empty slice fast-path.
        if slice_len == 0 {
            return [&mut [], &mut []];
        }

        // Note: this assertion is very important, it hints the compiler
        // which generates ~10% faster code than without it.
        assert(offset + slice_len <= self.capacity());

        // End offset is the last offset (exclusive) for our slice.
        // We use exclusive because it makes the math easier and it
        // matches Zigs slicing parameterization.
        let end_offset = offset + slice_len;

        // If our slice can't fit it in our length, then we need to advance.
        if end_offset > self.len() {
            self.advance(end_offset - self.len());
        }

        // Our start and end indexes into the storage buffer
        let start_idx = self.storage_offset(offset);
        let end_idx = self.storage_offset(end_offset - 1);

        // Optimistically, our data fits in one slice
        if end_idx >= start_idx {
            return [&mut self.storage[start_idx..end_idx + 1], &mut []];
        }

        let (front, back) = self.storage.split_at_mut(start_idx);
        [back, &mut front[..end_idx + 1]]
    }

    /// Advances the head/tail so that we can store amount.
    fn advance(&mut self, amount: usize) {
        assert(amount <= self.storage.len() - self.len());

        // Optimistically add our amount
        self.head += amount;

        // If we exceeded the length of the buffer, wrap around.
        if self.head >= self.storage.len() {
            self.head -= self.storage.len();
        }

        // If we're full, we have to keep tail lined up.
        if self.full {
            self.tail = self.head;
        }

        // We're full if the head reached the tail. The head can never
        // pass the tail because advance asserts amount is only in
        // available space left
        self.full = self.head == self.tail;
    }

    /// For a given offset from zero, this returns the offset in the
    /// storage buffer where this data can be found.
    fn storage_offset(&self, offset: usize) -> usize {
        assert(offset < self.storage.len());

        // This should be subtraction ideally to avoid overflows but
        // it would take a really, really, huge buffer to overflow.
        let fits_offset = self.tail + offset;
        if fits_offset < self.storage.len() {
            return fits_offset;
        }
        fits_offset - self.storage.len()
    }
}

impl<T: Copy> CircBuf<T> {
    /// Append a slice of `Copy` values using a plain memcpy.
    pub fn append_slice_assume_capacity_copy(&mut self, slice: &[T]) {
        let len = self.len();
        let [a, b] = self.get_ptr_slice(len, slice.len());
        let a_len = a.len();
        fastmem::copy(a, &slice[..a_len]);
        fastmem::copy(b, &slice[a_len..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Buf = CircBuf<u8>;

    #[test]
    fn basic() {
        let buf = Buf::init(12, 0);
        assert!(buf.empty());
        assert_eq!(0usize, buf.len());
    }

    #[test]
    fn circ_buf_append() {
        let mut buf = Buf::init(3, 0);

        buf.append(1).unwrap();
        buf.append(2).unwrap();
        buf.append(3).unwrap();
        assert_eq!(Err(Error::OutOfMemory), buf.append(4));
        buf.delete_oldest(1);
        buf.append(4).unwrap();
        assert_eq!(Err(Error::OutOfMemory), buf.append(5));
    }

    #[test]
    fn circ_buf_forward_iterator() {
        let mut buf = Buf::init(3, 0);

        // Empty
        {
            let mut it = buf.iterator(Direction::Forward);
            assert!(it.next().is_none());
        }

        // Partially full
        buf.append(1).unwrap();
        buf.append(2).unwrap();
        {
            let mut it = buf.iterator(Direction::Forward);
            assert_eq!(*it.next().unwrap(), 1);
            assert_eq!(*it.next().unwrap(), 2);
            assert!(it.next().is_none());
        }

        // Full
        buf.append(3).unwrap();
        {
            let mut it = buf.iterator(Direction::Forward);
            assert_eq!(*it.next().unwrap(), 1);
            assert_eq!(*it.next().unwrap(), 2);
            assert_eq!(*it.next().unwrap(), 3);
            assert!(it.next().is_none());
        }

        // Delete and add
        buf.delete_oldest(1);
        buf.append(4).unwrap();
        {
            let mut it = buf.iterator(Direction::Forward);
            assert_eq!(*it.next().unwrap(), 2);
            assert_eq!(*it.next().unwrap(), 3);
            assert_eq!(*it.next().unwrap(), 4);
            assert!(it.next().is_none());
        }
    }

    #[test]
    fn circ_buf_reverse_iterator() {
        let mut buf = Buf::init(3, 0);

        // Empty
        {
            let mut it = buf.iterator(Direction::Reverse);
            assert!(it.next().is_none());
        }

        // Partially full
        buf.append(1).unwrap();
        buf.append(2).unwrap();
        {
            let mut it = buf.iterator(Direction::Reverse);
            assert_eq!(*it.next().unwrap(), 2);
            assert_eq!(*it.next().unwrap(), 1);
            assert!(it.next().is_none());
        }

        // Full
        buf.append(3).unwrap();
        {
            let mut it = buf.iterator(Direction::Reverse);
            assert_eq!(*it.next().unwrap(), 3);
            assert_eq!(*it.next().unwrap(), 2);
            assert_eq!(*it.next().unwrap(), 1);
            assert!(it.next().is_none());
        }

        // Delete and add
        buf.delete_oldest(1);
        buf.append(4).unwrap();
        {
            let mut it = buf.iterator(Direction::Reverse);
            assert_eq!(*it.next().unwrap(), 4);
            assert_eq!(*it.next().unwrap(), 3);
            assert_eq!(*it.next().unwrap(), 2);
            assert!(it.next().is_none());
        }
    }

    #[test]
    fn circ_buf_first_last() {
        let mut buf = Buf::init(3, 0);

        buf.append(1).unwrap();
        buf.append(2).unwrap();
        buf.append(3).unwrap();
        assert_eq!(3, *buf.last().unwrap());
        assert_eq!(1, *buf.first().unwrap());
    }

    #[test]
    fn circ_buf_first_last_empty() {
        let buf = Buf::init(0, 0);

        assert!(buf.first().is_none());
        assert!(buf.last().is_none());
    }

    #[test]
    fn circ_buf_first_last_empty_with_cap() {
        let buf = Buf::init(3, 0);

        assert!(buf.first().is_none());
        assert!(buf.last().is_none());
    }

    #[test]
    fn circ_buf_append_slice() {
        let mut buf = Buf::init(5, 0);

        buf.append_slice_assume_capacity(b"hello");
        {
            let mut it = buf.iterator(Direction::Forward);
            assert_eq!(*it.next().unwrap(), b'h');
            assert_eq!(*it.next().unwrap(), b'e');
            assert_eq!(*it.next().unwrap(), b'l');
            assert_eq!(*it.next().unwrap(), b'l');
            assert_eq!(*it.next().unwrap(), b'o');
            assert!(it.next().is_none());
        }
    }

    #[test]
    fn circ_buf_append_slice_with_wrap() {
        let mut buf = Buf::init(4, 0);

        // Fill the buffer
        let cap = buf.capacity();
        let _ = buf.get_ptr_slice(0, cap);
        assert!(buf.full);
        assert_eq!(4usize, buf.len());

        // Delete
        buf.delete_oldest(2);
        assert!(!buf.full);
        assert_eq!(2usize, buf.len());

        buf.append_slice_assume_capacity_copy(b"AB");
        {
            let mut it = buf.iterator(Direction::Forward);
            assert_eq!(*it.next().unwrap(), 0);
            assert_eq!(*it.next().unwrap(), 0);
            assert_eq!(*it.next().unwrap(), b'A');
            assert_eq!(*it.next().unwrap(), b'B');
            assert!(it.next().is_none());
        }
    }

    #[test]
    fn circ_buf_get_ptr_slice_fits() {
        let mut buf = Buf::init(12, 0);

        let slices = buf.get_ptr_slice(0, 11);
        assert_eq!(11usize, slices[0].len());
        assert_eq!(0usize, slices[1].len());
        assert_eq!(11usize, buf.len());
    }

    #[test]
    fn circ_buf_get_ptr_slice_wraps() {
        let mut buf = Buf::init(4, 0);

        // Fill the buffer
        let cap = buf.capacity();
        let _ = buf.get_ptr_slice(0, cap);
        assert!(buf.full);
        assert_eq!(4usize, buf.len());

        // Delete
        buf.delete_oldest(2);
        assert!(!buf.full);
        assert_eq!(2usize, buf.len());

        // Get a slice that doesn't grow
        {
            let slices = buf.get_ptr_slice(0, 2);
            assert_eq!(2usize, slices[0].len());
            assert_eq!(0usize, slices[1].len());
            slices[0][0] = 1;
            slices[0][1] = 2;
            assert_eq!(2usize, buf.len());
        }

        // Get a slice that does grow, and forces wrap
        {
            let slices = buf.get_ptr_slice(2, 2);
            assert_eq!(2usize, slices[0].len());
            assert_eq!(0usize, slices[1].len());

            // should be empty
            assert_eq!(0u8, slices[0][0]);
            assert_eq!(0u8, slices[0][1]);
            slices[0][0] = 3;
            slices[0][1] = 4;
            assert_eq!(4usize, buf.len());
        }

        // Get a slice across boundaries
        {
            let slices = buf.get_ptr_slice(0, 4);
            assert_eq!(2usize, slices[0].len());
            assert_eq!(2usize, slices[1].len());

            assert_eq!(1u8, slices[0][0]);
            assert_eq!(2u8, slices[0][1]);
            assert_eq!(3u8, slices[1][0]);
            assert_eq!(4u8, slices[1][1]);
            assert_eq!(4usize, buf.len());
        }
    }

    #[test]
    fn circ_buf_rotate_to_zero() {
        let mut buf = Buf::init(12, 0);

        let _ = buf.get_ptr_slice(0, 11);
        buf.rotate_to_zero();
    }

    #[test]
    fn circ_buf_rotate_to_zero_offset() {
        let mut buf = Buf::init(4, 0);

        // Fill the buffer
        let _ = buf.get_ptr_slice(0, 3);
        assert_eq!(3usize, buf.len());

        // Delete
        buf.delete_oldest(2);
        assert!(!buf.full);
        assert_eq!(1usize, buf.len());
        assert!(buf.tail > 0 && buf.head >= buf.tail);

        // Rotate to zero
        buf.rotate_to_zero();
        assert_eq!(0usize, buf.tail);
        assert_eq!(1usize, buf.head);
    }

    #[test]
    fn circ_buf_rotate_to_zero_wraps() {
        let mut buf = Buf::init(4, 0);

        // Fill the buffer
        let _ = buf.get_ptr_slice(0, 3);
        assert_eq!(3usize, buf.len());
        assert!(buf.tail == 0 && buf.head == 3);

        // Delete all
        buf.delete_oldest(3);
        assert_eq!(0usize, buf.len());
        assert!(buf.tail == 3 && buf.head == 3);

        // Refill to force a wrap
        {
            let slices = buf.get_ptr_slice(0, 3);
            slices[0][0] = 1;
            slices[1][0] = 2;
            slices[1][1] = 3;
            assert_eq!(3usize, buf.len());
            assert!(buf.tail == 3 && buf.head == 2);
        }

        // Rotate to zero
        buf.rotate_to_zero();
        assert_eq!(0usize, buf.tail);
        assert_eq!(3usize, buf.head);
        {
            let slices = buf.get_ptr_slice(0, 3);
            assert_eq!(1u8, slices[0][0]);
            assert_eq!(2u8, slices[0][1]);
            assert_eq!(3u8, slices[0][2]);
        }
    }

    #[test]
    fn circ_buf_rotate_to_zero_full_no_wrap() {
        let mut buf = Buf::init(4, 0);

        // Fill the buffer
        let _ = buf.get_ptr_slice(0, 3);

        // Delete all
        buf.delete_oldest(3);

        // Refill to force a wrap
        {
            let slices = buf.get_ptr_slice(0, 4);
            slices[0][0] = 1;
            slices[1][0] = 2;
            slices[1][1] = 3;
            slices[1][2] = 4;
            assert!(buf.full);
        }

        // Rotate to zero
        buf.rotate_to_zero();
        assert!(buf.full);
        assert_eq!(0usize, buf.tail);
        assert_eq!(0usize, buf.head);
        {
            let slices = buf.get_ptr_slice(0, 4);
            assert_eq!(1u8, slices[0][0]);
            assert_eq!(2u8, slices[0][1]);
            assert_eq!(3u8, slices[0][2]);
            assert_eq!(4u8, slices[0][3]);
        }
    }

    #[test]
    fn circ_buf_resize_grow_from_zero() {
        let mut buf = Buf::init(0, 0);
        assert!(buf.full);

        // Resize
        buf.resize(2);
        assert!(!buf.full);
        assert_eq!(0usize, buf.len());
        assert_eq!(2usize, buf.capacity());

        buf.append(1).unwrap();
        buf.append(2).unwrap();

        {
            let slices = buf.get_ptr_slice(0, 2);
            assert_eq!(1u8, slices[0][0]);
            assert_eq!(2u8, slices[0][1]);
        }
    }

    #[test]
    fn circ_buf_resize_grow() {
        let mut buf = Buf::init(4, 0);

        // Fill and write
        {
            let slices = buf.get_ptr_slice(0, 4);
            slices[0][0] = 1;
            slices[0][1] = 2;
            slices[0][2] = 3;
            slices[0][3] = 4;
            assert!(buf.full);
        }

        // Resize
        buf.resize(6);
        assert!(!buf.full);
        assert_eq!(4usize, buf.len());
        assert_eq!(6usize, buf.capacity());

        {
            let slices = buf.get_ptr_slice(0, 4);
            assert_eq!(1u8, slices[0][0]);
            assert_eq!(2u8, slices[0][1]);
            assert_eq!(3u8, slices[0][2]);
            assert_eq!(4u8, slices[0][3]);
        }
    }

    #[test]
    fn circ_buf_resize_shrink() {
        let mut buf = Buf::init(4, 0);

        // Fill and write
        {
            let slices = buf.get_ptr_slice(0, 4);
            slices[0][0] = 1;
            slices[0][1] = 2;
            slices[0][2] = 3;
            slices[0][3] = 4;
            assert!(buf.full);
        }

        // Resize
        buf.resize(3);
        assert!(buf.full);
        assert_eq!(3usize, buf.len());
        assert_eq!(3usize, buf.capacity());

        {
            let slices = buf.get_ptr_slice(0, 3);
            assert_eq!(1u8, slices[0][0]);
            assert_eq!(2u8, slices[0][1]);
            assert_eq!(3u8, slices[0][2]);
        }
    }

    #[test]
    fn circ_buf_resize_shrink_partially_full() {
        let mut buf = Buf::init(5, 0);

        buf.append(1).unwrap();
        buf.append(2).unwrap();
        buf.append(3).unwrap();

        buf.resize(2);
        assert!(buf.full);
        assert_eq!(2usize, buf.len());
        assert_eq!(2usize, buf.capacity());

        let mut it = buf.iterator(Direction::Forward);
        assert_eq!(1u8, *it.next().unwrap());
        assert_eq!(2u8, *it.next().unwrap());
        assert!(it.next().is_none());
        assert_eq!(Err(Error::OutOfMemory), buf.append(4));
    }

    #[test]
    fn circ_buf_resize_shrink_to_length() {
        let mut buf = Buf::init(5, 0);

        buf.append(1).unwrap();
        buf.append(2).unwrap();
        buf.resize(2);

        assert!(buf.full);
        assert_eq!(2usize, buf.len());
        assert_eq!(2usize, buf.capacity());
    }

    #[test]
    fn circ_buf_resize_empty_to_zero() {
        let mut buf = Buf::init(5, 0);

        buf.resize(0);
        assert!(buf.full);
        assert_eq!(0usize, buf.len());
        assert_eq!(0usize, buf.capacity());
    }

    #[test]
    fn circ_buf_append_empty_slice() {
        let mut buf = Buf::init(5, 0);

        // Appending an empty slice to empty buffer should be a no-op
        buf.append_slice_assume_capacity(b"");
        assert_eq!(0usize, buf.len());
        assert!(!buf.full);

        // Buffer should still work normally after appending empty slice
        buf.append_slice_assume_capacity(b"hi");
        assert_eq!(2usize, buf.len());

        // Appending an empty slice to non-empty buffer should also be a no-op
        buf.append_slice_assume_capacity(b"");
        assert_eq!(2usize, buf.len());
    }

    #[test]
    fn circ_buf_get_ptr_slice_zero_length() {
        let mut buf = Buf::init(5, 0);

        // getPtrSlice with zero length on empty buffer should return empty slices
        let slices = buf.get_ptr_slice(0, 0);
        assert_eq!(0usize, slices[0].len());
        assert_eq!(0usize, slices[1].len());
        assert_eq!(0usize, buf.len());

        // Fill buffer partially
        buf.append_slice_assume_capacity(b"abc");
        assert_eq!(3usize, buf.len());

        // getPtrSlice with zero length on non-empty buffer should also work
        let slices2 = buf.get_ptr_slice(0, 0);
        assert_eq!(0usize, slices2[0].len());
        assert_eq!(0usize, slices2[1].len());
        assert_eq!(3usize, buf.len());
    }

    #[test]
    fn circ_buf_delete_oldest_zero() {
        let mut buf = Buf::init(5, 0);

        // deleteOldest(0) on empty buffer should be a no-op
        buf.delete_oldest(0);
        assert_eq!(0usize, buf.len());

        // Fill buffer
        buf.append_slice_assume_capacity(b"hello");
        assert_eq!(5usize, buf.len());

        // deleteOldest(0) on non-empty buffer should be a no-op
        buf.delete_oldest(0);
        assert_eq!(5usize, buf.len());

        // Verify data is unchanged
        let mut it = buf.iterator(Direction::Forward);
        assert_eq!(b'h', *it.next().unwrap());
    }
}
