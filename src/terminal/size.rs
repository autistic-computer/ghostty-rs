//! Port of `src/terminal/size.zig`.
//!
//! STATUS: ported
//!
//! Offset-based addressing for Ghostty's terminal page memory. A page is
//! one contiguous, relocatable buffer and every structure inside it refers
//! to other data through an [`Offset`] from the page's base address, so the
//! whole buffer can be copied with `memcpy` and keep working.
//!
//! Zig's `base: anytype` parameters (a many-pointer, a slice or an
//! [`OffsetBuf`]) are modelled with the [`Base`] trait.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ptr::NonNull;

/// The maximum size of a page in bytes. We use a u16 here because any
/// smaller bit size by Zig is upgraded anyways to a u16 on mainstream
/// CPU architectures, and because 65KB is a reasonable page size. To
/// support better configurability, we derive everything from this.
pub const MAX_PAGE_SIZE: usize = u32::MAX as usize;

/// The int type that can contain the maximum memory offset in bytes,
/// derived from the maximum terminal page size.
///
/// (`std.math.IntFittingRange(0, max_page_size - 1)`, which is `u32`.)
pub type OffsetInt = u32;

// Int types for maximum values of things. A lot of these sizes are
// based on "X is enough for any reasonable use case" principles.
// The goal is that a user can have the maxInt amount of all of these
// present at one time and be able to address them in a single Page.zig.

/// Total number of cells that are possible in each dimension (row/col).
/// Based on 2^16 being enough for any reasonable terminal size and allowing
/// IDs to remain 16-bit.
pub type CellCountInt = u16;

/// Total number of styles and hyperlinks that are possible in a page.
/// We match CellCountInt here because each cell in a single row can have at
/// most one style, making it simple to split a page by splitting rows.
///
/// Note due to the way RefCountedSet works, we are short one value, but
/// this is a theoretical limit we accept. A page with a single row max
/// columns wide would be one short of having every cell have a unique style.
pub type StyleCountInt = CellCountInt;
/// See [`StyleCountInt`].
pub type HyperlinkCountInt = CellCountInt;

/// Total number of bytes that can be taken up by grapheme data and string
/// data. Both of these technically unlimited with malicious input, but
/// we choose a reasonable limit of 2^32 (4GB) per.
pub type GraphemeBytesInt = u32;
/// See [`GraphemeBytesInt`].
pub type StringBytesInt = u32;

/// Allocation failure (`std.mem.Allocator.Error`), shared by the
/// fixed-capacity, offset-based structures that live in page memory
/// ([`crate::terminal::bitmap_allocator`], [`crate::terminal::hash_map`]).
/// Callers use it as the signal to grow the page and try again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AllocError {
    OutOfMemory,
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AllocError::OutOfMemory => f.write_str("OutOfMemory"),
        }
    }
}

impl std::error::Error for AllocError {}

/// `std.mem.alignForward`: round `value` up to a multiple of `alignment`,
/// which must be a power of two.
#[inline]
pub const fn align_forward(value: usize, alignment: usize) -> usize {
    debug_assert!(alignment.is_power_of_two());
    (value + (alignment - 1)) & !(alignment - 1)
}

/// Something that can be used as the base address for offset
/// calculations. This is the Rust spelling of Zig's `base: anytype`
/// (`intFromBase`): pointers, slices and [`OffsetBuf`] (whose *true base*
/// is used).
///
/// Producing a base pointer is always safe; dereferencing anything
/// computed from it is not.
pub trait Base {
    /// Returns the base address as a byte pointer (preserving provenance).
    fn base_ptr(self) -> *mut u8;
}

impl<T> Base for *mut T {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.cast()
    }
}

impl<T> Base for *const T {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.cast_mut().cast()
    }
}

impl<T> Base for NonNull<T> {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.as_ptr().cast()
    }
}

impl<T> Base for &[T] {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.as_ptr().cast_mut().cast()
    }
}

impl<T> Base for &mut [T] {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.as_mut_ptr().cast()
    }
}

impl Base for OffsetBuf {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.base
    }
}

impl Base for &OffsetBuf {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.base
    }
}

/// The offset from the base address of the page to the start of some data.
/// This is typed for ease of use.
///
/// In Zig this is a `packed struct(OffsetInt)` so methods can be attached
/// to an int; here it is a `repr(transparent)` wrapper with the same size.
#[repr(transparent)]
pub struct Offset<T> {
    pub offset: OffsetInt,
    _type: PhantomData<fn() -> T>,
}

impl<T> Offset<T> {
    /// Create an offset with the given byte offset.
    #[inline]
    pub const fn new(offset: OffsetInt) -> Self {
        Self {
            offset,
            _type: PhantomData,
        }
    }

    /// Returns a pointer to the start of the data, properly typed.
    ///
    /// This only computes an address (with wrapping arithmetic, keeping
    /// the base's provenance); it is safe to call, but the result may only
    /// be dereferenced if `base` really is the base of the buffer this
    /// offset was created for.
    #[inline]
    pub fn ptr(self, base: impl Base) -> *mut T {
        // The offset must be properly aligned for the type since
        // our return type is naturally aligned. We COULD modify this
        // to return arbitrary alignment, but its not something we need.
        let p = base.base_ptr().wrapping_add(self.offset as usize);
        debug_assert!((p as usize) % core::mem::align_of::<T>() == 0);
        p.cast()
    }
}

impl<T> Default for Offset<T> {
    #[inline]
    fn default() -> Self {
        Self::new(0)
    }
}

impl<T> Clone for Offset<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Offset<T> {}

impl<T> PartialEq for Offset<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
    }
}

impl<T> Eq for Offset<T> {}

impl<T> Hash for Offset<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.offset.hash(state);
    }
}

impl<T> fmt::Debug for Offset<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Offset").field("offset", &self.offset).finish()
    }
}

/// A slice of type T that stores via a base offset and len.
/// (Zig: `Offset(T).Slice`.)
pub struct OffsetSlice<T> {
    pub offset: Offset<T>,
    pub len: usize,
}

impl<T> OffsetSlice<T> {
    /// Create a slice from an offset and a length.
    #[inline]
    pub const fn new(offset: Offset<T>, len: usize) -> Self {
        Self { offset, len }
    }

    /// Returns a raw slice pointer for the data, properly typed.
    #[inline]
    pub fn slice_ptr(self, base: impl Base) -> *mut [T] {
        core::ptr::slice_from_raw_parts_mut(self.offset.ptr(base), self.len)
    }

    /// Returns a slice for the data, properly typed.
    ///
    /// # Safety
    ///
    /// `base` must be the base of the buffer this slice was created in,
    /// the `len` elements must be initialized and in bounds, and no mutable
    /// access to them may happen during `'a`.
    #[inline]
    pub unsafe fn slice<'a>(self, base: impl Base) -> &'a [T] {
        // SAFETY: guaranteed by the caller.
        unsafe { &*self.slice_ptr(base) }
    }

    /// Mutable variant of [`Self::slice`].
    ///
    /// # Safety
    ///
    /// Same as [`Self::slice`], plus no other access to the elements may
    /// happen during `'a`.
    #[inline]
    pub unsafe fn slice_mut<'a>(self, base: impl Base) -> &'a mut [T] {
        // SAFETY: guaranteed by the caller.
        unsafe { &mut *self.slice_ptr(base) }
    }
}

impl<T> Default for OffsetSlice<T> {
    #[inline]
    fn default() -> Self {
        Self {
            offset: Offset::default(),
            len: 0,
        }
    }
}

impl<T> Clone for OffsetSlice<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for OffsetSlice<T> {}

impl<T> PartialEq for OffsetSlice<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset && self.len == other.len
    }
}

impl<T> Eq for OffsetSlice<T> {}

impl<T> fmt::Debug for OffsetSlice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OffsetSlice")
            .field("offset", &self.offset.offset)
            .field("len", &self.len)
            .finish()
    }
}

/// Represents a buffer that is offset from some base pointer.
/// Offset-based structures should use this as their initialization
/// parameter so that they can know what segment of memory they own
/// while at the same time initializing their offset fields to be
/// against the true base.
///
/// The term "true base" is used to describe the base address of
/// the allocation, which i.e. can include memory that you do NOT
/// own and is used by some other structures. All offsets are against
/// this "true base" so that to determine addresses structures don't
/// need to add up all the intermediary offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetBuf {
    /// The true base pointer to the backing memory. This is
    /// "byte zero" of the allocation. This plus the offset make
    /// it easy to pass in the base pointer in all usage to this
    /// structure and the offsets are correct.
    pub base: *mut u8,

    /// Offset from base where the beginning of /this/ data
    /// structure is located. We use this so that we can slowly
    /// build up a chain of offset-based structures but always
    /// have the base pointer sent into functions be the true base.
    pub offset: usize,
}

impl OffsetBuf {
    /// Initialize a zero-offset buffer from a base.
    #[inline]
    pub fn init(base: impl Base) -> OffsetBuf {
        Self::init_offset(base, 0)
    }

    /// Initialize from some base pointer and offset.
    #[inline]
    pub fn init_offset(base: impl Base, offset: usize) -> OffsetBuf {
        OffsetBuf {
            base: base.base_ptr(),
            offset,
        }
    }

    /// The base address for the start of the data for the user
    /// of this OffsetBuf. This is where your data structure should
    /// begin; anything before this is NOT your memory.
    #[inline]
    pub fn start(&self) -> *mut u8 {
        self.base.wrapping_add(self.offset)
    }

    /// Returns an Offset calculation for some child member of
    /// your struct. The offset is against the true base pointer
    /// so that future callers can pass that in as the base.
    #[inline]
    pub fn member<T>(&self, len: usize) -> Offset<T> {
        let off = self.offset + len;
        debug_assert!(off <= OffsetInt::MAX as usize);
        Offset::new(off as OffsetInt)
    }

    /// Add an offset to the current offset.
    #[inline]
    pub fn add(&self, offset: usize) -> OffsetBuf {
        OffsetBuf {
            base: self.base,
            offset: self.offset + offset,
        }
    }

    /// Rebase the offset to have a zero offset by rebasing onto start.
    /// This is similar to `add` but all of the offsets are merged into base.
    #[inline]
    pub fn rebase(&self, offset: usize) -> OffsetBuf {
        OffsetBuf {
            base: self.start().wrapping_add(offset),
            offset: 0,
        }
    }
}

/// Get the offset for a given type from some base pointer to the
/// actual pointer to the type.
#[inline]
pub fn get_offset<T>(base: impl Base, ptr: *const T) -> Offset<T> {
    let base_int = base.base_ptr() as usize;
    let ptr_int = ptr as usize;
    let offset = ptr_int - base_int;
    debug_assert!(offset <= OffsetInt::MAX as usize);
    Offset::new(offset as OffsetInt)
}

/// A heap buffer with a given alignment, used by tests in place of
/// Zig's `testing.allocator.alignedAlloc(u8, align, len)`.
///
/// The contents are filled with `0xAA` (like Zig's "undefined" memory) so
/// that code which forgets to initialize something is more likely to be
/// caught.
#[cfg(test)]
pub(crate) struct AlignedBuf {
    ptr: NonNull<u8>,
    len: usize,
    layout: std::alloc::Layout,
}

#[cfg(test)]
impl AlignedBuf {
    pub(crate) fn new(len: usize, align: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(len.max(1), align).unwrap();
        // SAFETY: layout has a non-zero size.
        let raw = unsafe { std::alloc::alloc(layout) };
        let ptr = NonNull::new(raw).unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        // SAFETY: ptr is valid for layout.size() bytes.
        unsafe { core::ptr::write_bytes(ptr.as_ptr(), 0xAA, layout.size()) };
        Self { ptr, len, layout }
    }

    pub(crate) fn ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { std::alloc::dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset() {
        // This test is here so that if Offset changes, we can be very aware
        // of this effect and think about the implications of it.
        assert_eq!(core::mem::size_of::<OffsetInt>(), 4);
        assert_eq!(OffsetInt::MAX, u32::MAX);
        assert_eq!(core::mem::size_of::<Offset<u64>>(), 4);
    }

    #[test]
    fn offset_ptr_u8() {
        let offset: Offset<u8> = Offset::new(42);
        let base: *const Offset<u8> = &offset;
        let base_int = base as usize;
        let actual = offset.ptr(base);
        assert_eq!(base_int + 42, actual as usize);
    }

    #[test]
    fn offset_ptr_structural() {
        #[allow(dead_code)]
        struct Struct {
            x: u32,
            y: u32,
        }
        let align = core::mem::align_of::<Struct>();
        let offset: Offset<Struct> = Offset::new((align * 4) as OffsetInt);
        let off_ptr: *const Offset<Struct> = &offset;
        let base_int = align_forward(off_ptr as usize, align);
        // Derive the base from the local's address so it keeps a provenance;
        // it is never dereferenced.
        let base: *mut u8 = off_ptr
            .cast::<u8>()
            .cast_mut()
            .wrapping_add(base_int - off_ptr as usize);
        let actual = offset.ptr(base);
        assert_eq!(base_int + offset.offset as usize, actual as usize);
    }

    #[test]
    fn get_offset_bytes() {
        let widgets: &[u8] = b"ABCD";
        let offset = get_offset(widgets.as_ptr(), &widgets[2]);
        assert_eq!(2 as OffsetInt, offset.offset);
    }

    #[test]
    fn get_offset_structs() {
        #[allow(dead_code)]
        struct Widget {
            x: u32,
            y: u32,
        }
        let widgets: &[Widget] = &[
            Widget { x: 1, y: 2 },
            Widget { x: 3, y: 4 },
            Widget { x: 5, y: 6 },
            Widget { x: 7, y: 8 },
            Widget { x: 9, y: 10 },
        ];
        let offset = get_offset(widgets.as_ptr(), &widgets[2]);
        assert_eq!(
            (core::mem::size_of::<Widget>() * 2) as OffsetInt,
            offset.offset
        );
    }
}
