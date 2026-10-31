//! Port of `src/fastmem.zig`.
//!
//! STATUS: ported
//!
//! Rust's `ptr::copy`/`ptr::copy_nonoverlapping` lower to the libc
//! `memmove`/`memcpy` symbols already, which is what the Zig version
//! goes out of its way to call.

use std::ptr;

/// Same as @memmove but prefers libc memmove if it is
/// available because it is generally much faster?.
///
/// Rust's borrow rules prevent `dest` and `source` from overlapping here;
/// use [`move_within`] for overlapping moves inside one slice.
#[inline(always)]
pub fn r#move<T: Copy>(dest: &mut [T], source: &[T]) {
    assert!(dest.len() >= source.len());
    // SAFETY: bounds checked above; T: Copy so bitwise copies are valid.
    unsafe { ptr::copy(source.as_ptr(), dest.as_mut_ptr(), source.len()) }
}

/// memmove `items[src]` to `items[dest..]` within one slice (the ranges
/// may overlap). Equivalent to `slice::copy_within`.
#[inline(always)]
pub fn move_within<T: Copy>(items: &mut [T], src: std::ops::Range<usize>, dest: usize) {
    items.copy_within(src, dest);
}

/// Same as @memcpy but prefers libc memcpy if it is available
/// because it is generally much faster.
#[inline(always)]
pub fn copy<T: Copy>(dest: &mut [T], source: &[T]) {
    dest[..source.len()].copy_from_slice(source);
}

/// Shift `items[1..]` down by one slot (memmove). The value in the last
/// slot is left as a bitwise duplicate that the caller must overwrite.
///
/// # Safety
/// `items` must not be empty and the caller must overwrite the last
/// element with `ptr::write` before it is used or dropped.
#[inline(always)]
unsafe fn shift_down<T>(items: &mut [T]) {
    let len = items.len();
    let p = items.as_mut_ptr();
    ptr::copy(p.add(1), p, len - 1);
}

/// Shift `items[..len-1]` up by one slot (memmove). The value in the first
/// slot is left as a bitwise duplicate that the caller must overwrite.
///
/// # Safety
/// Same as [`shift_down`] but for the first element.
#[inline(always)]
unsafe fn shift_up<T>(items: &mut [T]) {
    let len = items.len();
    let p = items.as_mut_ptr();
    ptr::copy(p, p.add(1), len - 1);
}

/// Moves the first item to the end.
/// For the reverse of this, use `fastmem.rotateOnceR`.
///
/// Same as std.mem.rotate(T, items, 1) but more efficient by using memmove
/// and a tmp var for the single rotated item instead of 3 calls to reverse.
///
/// e.g. `0 1 2 3` -> `1 2 3 0`.
#[inline(always)]
pub fn rotate_once<T>(items: &mut [T]) {
    if items.is_empty() {
        return;
    }
    // SAFETY: non-empty; the temporarily duplicated slot is overwritten
    // before returning and nothing in between can panic.
    unsafe {
        let tmp = ptr::read(items.as_ptr());
        shift_down(items);
        let last = items.len() - 1;
        ptr::write(items.as_mut_ptr().add(last), tmp);
    }
}

/// Moves the last item to the start.
/// Reverse operation of `fastmem.rotateOnce`.
///
/// Same as std.mem.rotate(T, items, items.len - 1) but more efficient by
/// using memmove and a tmp var for the single rotated item instead of 3
/// calls to reverse.
///
/// e.g. `0 1 2 3` -> `3 0 1 2`.
#[inline(always)]
pub fn rotate_once_r<T>(items: &mut [T]) {
    if items.is_empty() {
        return;
    }
    // SAFETY: see rotate_once.
    unsafe {
        let last = items.len() - 1;
        let tmp = ptr::read(items.as_ptr().add(last));
        shift_up(items);
        ptr::write(items.as_mut_ptr(), tmp);
    }
}

/// Rotates a new item in to the end of a slice.
/// The first item from the slice is removed and returned.
///
/// e.g. rotating `4` in to `0 1 2 3` makes it `1 2 3 4` and returns `0`.
///
/// For the reverse of this, use `fastmem.rotateInR`.
#[inline(always)]
pub fn rotate_in<T>(items: &mut [T], item: T) -> T {
    assert!(!items.is_empty());
    // SAFETY: non-empty; the removed value is moved out and the duplicated
    // last slot is overwritten with `item`.
    unsafe {
        let removed = ptr::read(items.as_ptr());
        shift_down(items);
        let last = items.len() - 1;
        ptr::write(items.as_mut_ptr().add(last), item);
        removed
    }
}

/// Rotates a new item in to the start of a slice.
/// The last item from the slice is removed and returned.
///
/// e.g. rotating `4` in to `0 1 2 3` makes it `4 0 1 2` and returns `3`.
///
/// Reverse operation of `fastmem.rotateIn`.
#[inline(always)]
pub fn rotate_in_r<T>(items: &mut [T], item: T) -> T {
    assert!(!items.is_empty());
    // SAFETY: see rotate_in.
    unsafe {
        let last = items.len() - 1;
        let removed = ptr::read(items.as_ptr().add(last));
        shift_up(items);
        ptr::write(items.as_mut_ptr(), item);
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Zig file has no tests; these cover the rotate helpers since the
    // Rust versions are implemented with raw pointer moves.
    #[test]
    fn rotate_helpers() {
        let mut v = [0, 1, 2, 3];
        rotate_once(&mut v);
        assert_eq!(v, [1, 2, 3, 0]);
        rotate_once_r(&mut v);
        assert_eq!(v, [0, 1, 2, 3]);
        assert_eq!(rotate_in(&mut v, 4), 0);
        assert_eq!(v, [1, 2, 3, 4]);
        assert_eq!(rotate_in_r(&mut v, 9), 4);
        assert_eq!(v, [9, 1, 2, 3]);

        let mut s = vec![String::from("a"), String::from("b")];
        rotate_once(&mut s);
        assert_eq!(s, ["b", "a"]);
        let out = rotate_in(&mut s, String::from("c"));
        assert_eq!(out, "b");
        assert_eq!(s, ["a", "c"]);

        let mut d = [0u8; 4];
        copy(&mut d, &[1, 2]);
        assert_eq!(d, [1, 2, 0, 0]);
        r#move(&mut d[2..], &[7, 8]);
        assert_eq!(d, [1, 2, 7, 8]);
        move_within(&mut d, 0..3, 1);
        assert_eq!(d, [1, 1, 2, 7]);
    }
}
