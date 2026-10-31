//! Port of `src/terminal/hash_map.zig`.
//!
//! STATUS: ported
//!
//! This file contains a fork of the Zig stdlib HashMap implementation tuned
//! for use with our terminal page representation.
//!
//! The main goal we need to achieve that wasn't possible with the stdlib
//! HashMap is to utilize offsets rather than full pointers so that we can
//! copy around the entire backing memory and keep the hash map working.
//!
//! Additionally, for serialization/deserialization purposes, we need to be
//! able to create a HashMap instance and manually set the offsets up. The
//! stdlib HashMap does not export Metadata so this isn't possible.
//!
//! Also, I want to be able to understand possible capacity for a given K,V
//! type and fixed memory amount. The stdlib HashMap doesn't publish its
//! internal allocation size calculation.
//!
//! Finally, I removed many of the APIs that we'll never require for our
//! usage just so that this file is smaller, easier to understand, and has
//! less opportunity for bugs.
//!
//! Besides these shortcomings, the stdlib HashMap has some great qualities
//! that we want to keep, namely the fact that it is backed by a single large
//! allocation rather than pointers to separate allocations. This is important
//! because our terminal page representation is backed by a single large
//! allocation so we can give the HashMap a slice of memory to operate in.
//!
//! This fork diverges from the stdlib in one significant way: removal uses
//! backward-shift deletion (Knuth vol. 3, section 6.4, algorithm R) rather
//! than tombstones. A fixed-capacity map cannot outgrow tombstone buildup
//! the way an allocating map does, so tombstones require either unbounded
//! probe lengths or periodic in-place rebuilds with subtle bookkeeping.
//! Backward-shift deletion instead restores the table after every removal
//! to the exact state it would be in had the removed key never been
//! inserted. Probe chains are therefore always minimal for the insertion
//! order, there is no fragmentation to repair, and lookup cost depends only
//! on the current load factor.
//!
//! Pointer stability: insertion never moves existing entries, but removal
//! may move *other* entries within a probe cluster. Any key or value
//! pointers previously returned by the map must be considered invalidated
//! by any removal.
//!
//! # Rust port notes
//!
//! * Zig's comptime `Context` duck typing becomes the [`HashContext`]
//!   trait. `HashContext<K, Q>` hashes a lookup key `Q` and compares it to a
//!   stored key `K`; the `*Adapted` functions take any such context. The
//!   context-less functions (`put`, `get`, ...) require `C: Default`
//!   (Zig requires a zero-sized context for them).
//! * Keys and values are plain-old-data (`Copy`) because they live in
//!   relocatable page memory.
//! * Pointers handed out by the map (`Entry`, `GetOrPutResult`, ...) are raw
//!   pointers, like Zig's `*K`/`*V`; dereferencing them is up to the caller
//!   and they are invalidated by any removal (see above).
//! * [`HashMapUnmanaged`] holds raw pointers into the backing buffer; it is
//!   created with the `unsafe` functions [`HashMapUnmanaged::init`],
//!   [`HashMapUnmanaged::init_assume_zeroed`] or [`OffsetHashMap::map`],
//!   whose contract is that the buffer stays valid (and is not accessed
//!   through anything else) for as long as the map value is used.

use core::marker::PhantomData;

use crate::terminal::size::{align_forward, AllocError, Base, Offset, OffsetBuf};

/// The default allows every raw slot to be occupied. Callers whose maps see
/// removal-heavy churn should choose a lower value to bound probe lengths.
pub const DEFAULT_MAX_LOAD_PERCENTAGE: u8 = 100;

/// Hashing and equality for a map whose stored keys are `K`, looked up by
/// keys of type `Q` (Zig: the `Context` type / `ctx: anytype` of the
/// `*Adapted` functions).
pub trait HashContext<K, Q: ?Sized = K> {
    /// Return the 64-bit hash of `key`.
    fn hash(&self, key: &Q) -> u64;

    /// Return true if the lookup key `a` equals the stored key `b`.
    fn eql(&self, a: &Q, b: &K) -> bool;
}

/// Marker for types whose bytes uniquely represent their value (Zig's
/// `std.meta.hasUniqueRepresentation`): no padding and no two distinct
/// byte patterns for equal values. [`AutoContext`] hashes such keys by
/// their bytes.
///
/// # Safety
///
/// Implementors must have no padding bytes (all bytes initialized) and
/// `==` must coincide with byte equality.
pub unsafe trait UniqueRepr: Copy + PartialEq {}

macro_rules! unique_repr {
    ($($t:ty),* $(,)?) => { $(
        // SAFETY: primitive integers/bool/char have no padding and compare bytewise.
        unsafe impl UniqueRepr for $t {}
    )* };
}
unique_repr!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, bool, char);

// SAFETY: arrays of unique-representation elements have no padding.
unsafe impl<T: UniqueRepr, const N: usize> UniqueRepr for [T; N] {}

// SAFETY: `Offset<T>` is a `repr(transparent)` `u32` and compares by it.
unsafe impl<T> UniqueRepr for Offset<T> {}

/// The automatic context (`std.hash_map.AutoContext`-like): Wyhash of the
/// key bytes and `==` for equality.
///
/// NOTE: Zig falls back to `std.hash.autoHash` for keys without a unique
/// representation; this port only supports unique-representation keys
/// (which is all Ghostty uses).
pub struct AutoContext<K>(PhantomData<fn() -> K>);

impl<K> Default for AutoContext<K> {
    #[inline]
    fn default() -> Self {
        AutoContext(PhantomData)
    }
}

impl<K> Clone for AutoContext<K> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for AutoContext<K> {}

impl<K> core::fmt::Debug for AutoContext<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("AutoContext")
    }
}

impl<K: UniqueRepr> HashContext<K> for AutoContext<K> {
    #[inline(always)]
    fn hash(&self, key: &K) -> u64 {
        // SAFETY: `K: UniqueRepr` guarantees every byte is initialized.
        let bytes = unsafe {
            core::slice::from_raw_parts((key as *const K).cast::<u8>(), core::mem::size_of::<K>())
        };

        // On wasm32, Wyhash's 64x64 -> 128 multiplies lower to
        // __multi3 libcalls (wasm has no widening multiply
        // instruction), which makes the hash itself the
        // dominant cost of small-key map operations.
        //
        // Keys here fit in a u64, so use a splitmix64-style finalizer
        // instead: two native i64.mul with full avalanche in
        // both the low bits (slot index) and high bits
        // (metadata fingerprint).
        if cfg!(target_family = "wasm") && core::mem::size_of::<K>() <= 8 {
            let mut buf = [0u8; 8];
            buf[..bytes.len()].copy_from_slice(bytes);
            let mut x = u64::from_ne_bytes(buf);
            x ^= x >> 33;
            x = x.wrapping_mul(0xff51afd7ed558ccd);
            x ^= x >> 33;
            x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
            x ^= x >> 33;
            return x;
        }

        Wyhash::hash(0, bytes)
    }

    #[inline(always)]
    fn eql(&self, a: &K, b: &K) -> bool {
        a == b
    }
}

/// A HashMap type that uses offsets rather than pointers, making it
/// possible to efficiently move around the backing memory without
/// invalidating the HashMap.
pub struct OffsetHashMap<K, V, C, const MAX_LOAD_PERCENTAGE: u8> {
    /// The slot metadata in the backing memory. The map's size counter
    /// sits immediately before it (see `Header`).
    pub metadata: Offset<Metadata>,

    /// The key and value arrays in the backing memory.
    pub keys: Offset<K>,
    pub values: Offset<V>,

    /// The number of slots. This never changes after init, so it is
    /// kept here rather than in the backing memory.
    pub capacity: Size,

    _ctx: PhantomData<fn() -> C>,
}

/// `AutoOffsetHashMap(K, V, max_load_percentage)`.
pub type AutoOffsetHashMap<K, V, const MAX_LOAD_PERCENTAGE: u8> =
    OffsetHashMap<K, V, AutoContext<K>, MAX_LOAD_PERCENTAGE>;

/// `AutoHashMapUnmanaged(K, V, max_load_percentage)` (private in Zig).
pub type AutoHashMapUnmanaged<K, V, const MAX_LOAD_PERCENTAGE: u8> =
    HashMapUnmanaged<K, V, AutoContext<K>, MAX_LOAD_PERCENTAGE>;

impl<K, V, C, const P: u8> Default for OffsetHashMap<K, V, C, P> {
    fn default() -> Self {
        Self {
            metadata: Offset::default(),
            keys: Offset::default(),
            values: Offset::default(),
            capacity: 0,
            _ctx: PhantomData,
        }
    }
}

impl<K, V, C, const P: u8> Clone for OffsetHashMap<K, V, C, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, C, const P: u8> Copy for OffsetHashMap<K, V, C, P> {}

impl<K, V, C, const P: u8> core::fmt::Debug for OffsetHashMap<K, V, C, P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OffsetHashMap")
            .field("metadata", &self.metadata)
            .field("keys", &self.keys.offset)
            .field("values", &self.values.offset)
            .field("capacity", &self.capacity)
            .finish()
    }
}

impl<K: Copy, V: Copy, C: HashContext<K>, const P: u8> OffsetHashMap<K, V, C, P> {
    /// This is the alignment that the base pointer must have.
    pub const BASE_ALIGN: usize = HashMapUnmanaged::<K, V, C, P>::BASE_ALIGN;

    /// Returns the total size of the backing memory required for a
    /// HashMap with the given capacity. The base ptr must also be
    /// aligned to base_align.
    pub fn layout(cap: Size) -> Layout {
        HashMapUnmanaged::<K, V, C, P>::layout_for_size(cap)
    }

    /// Initialize a new HashMap with the given capacity and backing
    /// memory. The backing memory must be aligned to base_align.
    ///
    /// # Safety
    ///
    /// `buf.start()` must point to at least `l.total_size` writable bytes
    /// aligned to [`Self::BASE_ALIGN`].
    pub unsafe fn init(buf: OffsetBuf, l: Layout) -> Self {
        // SAFETY: forwarded caller contract.
        let this = unsafe { Self::init_assume_zeroed(buf, l) };
        // SAFETY: forwarded caller contract; `buf` is the true base.
        let mut m = unsafe { this.map(buf) };
        m.clear_retaining_capacity();
        this
    }

    /// Like `init`, but for backing memory that the caller guarantees
    /// is already zero-filled (e.g. fresh OS pages). This writes
    /// nothing to the backing memory: all-zero slot metadata means
    /// every slot is free and a zero size counter means empty, so the
    /// OS pages behind the map stay untouched until the first insert.
    ///
    /// # Safety
    ///
    /// Same as [`Self::init`], and the header and metadata region must be
    /// zero.
    pub unsafe fn init_assume_zeroed(buf: OffsetBuf, l: Layout) -> Self {
        debug_assert!((buf.start() as usize) % Self::BASE_ALIGN == 0);
        Self {
            metadata: buf.member::<Metadata>(l.metadata_start),
            keys: buf.member::<K>(l.keys_start),
            values: buf.member::<V>(l.vals_start),
            capacity: l.capacity,
            _ctx: PhantomData,
        }
    }

    /// Returns the pointer-based map from a base pointer.
    ///
    /// # Safety
    ///
    /// `base` must be the true base of the (initialized) buffer this map was
    /// laid out in. The returned map must not be used after that buffer is
    /// freed or moved, and must not be used concurrently with other
    /// accesses to the map's memory.
    pub unsafe fn map(&self, base: impl Base) -> HashMapUnmanaged<K, V, C, P> {
        let base = base.base_ptr();
        HashMapUnmanaged {
            metadata: self.metadata.ptr(base),
            keys: self.keys.ptr(base),
            values: self.values.ptr(base),
            cap: self.capacity,
            _ctx: PhantomData,
        }
    }
}

// This hashmap is specially designed for sizes that fit in a u32.
pub type Size = u32;

// u64 hashes guarantee us that the fingerprint bits will never be used
// to compute the index of a slot, maximizing the use of entropy.
pub type Hash = u64;

/// 7-bit fingerprint (Zig `u7`).
pub type FingerPrint = u8;

/// Metadata for a slot. It can be in two states: free or used.
/// To the used state, we add 7 bits from the slot's key hash. These
/// are used as a fast way to disambiguate between entries without
/// having to use the equality function. If two fingerprints are
/// different, we know that we don't have to compare the keys at all.
/// The 7 bits are the highest ones from a 64 bit hash. This way, not
/// only we use the `log2(capacity)` lowest bits from the hash to determine
/// a slot index, but we use 7 more bits to quickly resolve collisions
/// when multiple elements with different hashes end up wanting to be in the same slot.
/// Not using the equality function means we don't have to read into
/// the entries array, likely avoiding a cache miss and a potentially
/// costly function call.
///
/// Bit layout (Zig `packed struct(u8)`): bits 0..7 fingerprint, bit 7 used.
#[repr(transparent)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Metadata(pub u8);

impl Metadata {
    const USED_BIT: u8 = 0x80;
    const FP_MASK: u8 = 0x7f;

    #[inline]
    pub const fn fingerprint(self) -> FingerPrint {
        self.0 & Self::FP_MASK
    }

    #[inline]
    pub const fn used(self) -> u8 {
        self.0 >> 7
    }

    #[inline]
    pub const fn is_used(self) -> bool {
        self.used() == 1
    }

    #[inline]
    pub const fn is_free(self) -> bool {
        // A free slot is always the all-zero byte: `fill` sets the
        // used bit and removal zeroes the whole byte. Comparing the
        // full byte (rather than testing the used bit) lets the
        // optimizer fuse this with the fingerprint comparison in
        // probe loops into single-byte compares.
        self.0 == 0
    }

    #[inline]
    pub const fn take_fingerprint(hash: Hash) -> FingerPrint {
        const HASH_BITS: u32 = 64;
        const FP_BITS: u32 = 7;
        (hash >> (HASH_BITS - FP_BITS)) as FingerPrint
    }

    #[inline]
    pub fn fill(&mut self, fp: FingerPrint) {
        self.0 = Self::USED_BIT | (fp & Self::FP_MASK);
    }
}

const _: () = {
    assert!(core::mem::size_of::<Metadata>() == 1);
    assert!(core::mem::align_of::<Metadata>() == 1);
};

/// The part of the map's state that changes after init. It lives
/// in the backing buffer so that the map can be handed around by
/// value; its zero value is the empty map.
#[repr(C)]
struct Header {
    size: Size,
}

pub struct Entry<K, V> {
    pub key_ptr: *mut K,
    pub value_ptr: *mut V,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KV<K, V> {
    pub key: K,
    pub value: V,
}

pub struct GetOrPutResult<K, V> {
    pub key_ptr: *mut K,
    pub value_ptr: *mut V,
    pub found_existing: bool,
}

/// The memory layout for the underlying buffer for a given capacity.
/// All offsets are from the start of the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The total size of the buffer required. The buffer is expected
    /// to be aligned to `base_align`.
    pub total_size: usize,

    /// The offset to the start of the slot metadata. The header
    /// occupies the bytes before it.
    pub metadata_start: usize,

    /// The offset to the start of the keys data.
    pub keys_start: usize,

    /// The offset to the start of the values data.
    pub vals_start: usize,

    /// The capacity that was used to calculate this layout.
    pub capacity: Size,
}

/// Fork of stdlib.HashMap as of Zig 0.12 modified to use offsets for
/// the key/values pointer, and backward-shift deletion in place of
/// tombstones. The metadata is still a pointer to limit the amount of
/// arithmetic required to access it. See the file comment for full details.
///
/// The backing buffer holds a `Header` (the size counter) followed
/// by the `Metadata`s, then the keys and values arrays. Everything
/// the map needs that does not change after init, the capacity and
/// the entry pointers, lives in this struct instead of the buffer,
/// so that a zero-filled buffer is a valid empty map and init never
/// has to write to it.
///
/// # Safety invariant
///
/// `metadata`, `keys` and `values` point to `cap` slots each of a live
/// buffer laid out by [`Self::layout_for_capacity`]`(cap)`, and the
/// `Header` lives right before `metadata`. This is established by the
/// `unsafe` constructors.
pub struct HashMapUnmanaged<K, V, C, const MAX_LOAD_PERCENTAGE: u8> {
    /// Pointer to the slot metadata. The header sits right before it.
    pub metadata: *mut Metadata,

    /// The key and value arrays.
    pub keys: *mut K,
    pub values: *mut V,

    /// The number of slots. Always zero or a power of two.
    pub cap: Size,

    _ctx: PhantomData<fn() -> C>,
}

impl<K, V, C, const P: u8> Clone for HashMapUnmanaged<K, V, C, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, C, const P: u8> Copy for HashMapUnmanaged<K, V, C, P> {}

/// Iterates the entries of the map. Any mutation of the map
/// invalidates the iterator: removal may move entries across the
/// iteration cursor. (Zig: `HashMapUnmanaged.Iterator`.)
pub struct MapIterator<'a, K, V, C, const P: u8> {
    hm: &'a HashMapUnmanaged<K, V, C, P>,
    index: Size,
}

impl<K: Copy, V: Copy, C: HashContext<K>, const P: u8> Iterator for MapIterator<'_, K, V, C, P> {
    type Item = Entry<K, V>;

    fn next(&mut self) -> Option<Entry<K, V>> {
        debug_assert!(self.index <= self.hm.cap);
        if self.hm.count() == 0 {
            return None;
        }

        let cap = self.hm.cap;
        while self.index != cap {
            let idx = self.index as usize;
            // SAFETY: idx < cap, within the metadata array (type invariant).
            let md = unsafe { *self.hm.metadata.add(idx) };
            if md.is_used() {
                // SAFETY: idx < cap, within the key/value arrays.
                let key = unsafe { self.hm.keys.add(idx) };
                let value = unsafe { self.hm.values.add(idx) };
                self.index += 1;
                return Some(Entry {
                    key_ptr: key,
                    value_ptr: value,
                });
            }
            self.index += 1;
        }

        None
    }
}

/// Iterates one field (keys or values) of the used slots.
/// (Zig: `HashMapUnmanaged.FieldIterator(T)`.)
pub struct FieldIterator<T> {
    len: usize,
    metadata: *const Metadata,
    items: *mut T,
}

pub type KeyIterator<K> = FieldIterator<K>;
pub type ValueIterator<V> = FieldIterator<V>;

impl<T> Iterator for FieldIterator<T> {
    type Item = *mut T;

    fn next(&mut self) -> Option<*mut T> {
        while self.len > 0 {
            self.len -= 1;
            // SAFETY: `len` counts the remaining slots of the arrays this
            // iterator was created over.
            let used = unsafe { (*self.metadata).is_used() };
            let item = self.items;
            self.metadata = self.metadata.wrapping_add(1);
            self.items = self.items.wrapping_add(1);
            if used {
                return Some(item);
            }
        }
        None
    }
}

impl<K: Copy, V: Copy, C: HashContext<K>, const MAX_LOAD_PERCENTAGE: u8>
    HashMapUnmanaged<K, V, C, MAX_LOAD_PERCENTAGE>
{
    const CHECKS: () = {
        assert!(MAX_LOAD_PERCENTAGE > 0);
        assert!(MAX_LOAD_PERCENTAGE <= 100);
    };

    const HEADER_ALIGN: usize = core::mem::align_of::<Header>();
    const KEY_ALIGN: usize = if core::mem::size_of::<K>() == 0 {
        1
    } else {
        core::mem::align_of::<K>()
    };
    const VAL_ALIGN: usize = if core::mem::size_of::<V>() == 0 {
        1
    } else {
        core::mem::align_of::<V>()
    };

    /// This is the alignment that the base pointer must have.
    pub const BASE_ALIGN: usize = {
        let mut a = Self::HEADER_ALIGN;
        if Self::KEY_ALIGN > a {
            a = Self::KEY_ALIGN;
        }
        if Self::VAL_ALIGN > a {
            a = Self::VAL_ALIGN;
        }
        a
    };

    /// Initialize a hash map with a given capacity and a buffer. The
    /// buffer must fit within the size defined by `layoutForCapacity`.
    ///
    /// # Safety
    ///
    /// `buf.start()` must point to at least `layout.total_size` writable
    /// bytes aligned to [`Self::BASE_ALIGN`] that stay valid (and are not
    /// otherwise accessed) while the returned map is used.
    pub unsafe fn init(buf: OffsetBuf, layout: Layout) -> Self {
        // SAFETY: forwarded caller contract.
        let mut map = unsafe { Self::init_assume_zeroed(buf, layout) };
        map.clear_retaining_capacity();
        map
    }

    /// Like `init`, but for a buffer that the caller guarantees is
    /// already zero-filled. Nothing is written: an all-zero metadata
    /// byte is a free slot (see `Metadata.isFree`) and a zero header
    /// is an empty map.
    ///
    /// # Safety
    ///
    /// Same as [`Self::init`]; behavior is undefined if the header and
    /// metadata region is not zero.
    pub unsafe fn init_assume_zeroed(buf: OffsetBuf, layout: Layout) -> Self {
        #[allow(clippy::let_unit_value)]
        let () = Self::CHECKS;
        debug_assert!((buf.start() as usize) % Self::BASE_ALIGN == 0);
        Self {
            metadata: buf.start().wrapping_add(layout.metadata_start).cast(),
            keys: buf.member::<K>(layout.keys_start).ptr(buf),
            values: buf.member::<V>(layout.vals_start).ptr(buf),
            cap: layout.capacity,
            _ctx: PhantomData,
        }
    }

    pub fn ensure_total_capacity(&mut self, new_size: Size) -> Result<(), AllocError> {
        if new_size > self.count() {
            self.check_capacity(new_size - self.count())?;
        }
        Ok(())
    }

    pub fn ensure_unused_capacity(&mut self, additional_size: Size) -> Result<(), AllocError> {
        self.ensure_total_capacity(self.count() + additional_size)
    }

    pub fn clear_retaining_capacity(&mut self) {
        self.init_metadatas();
        // SAFETY: header is valid per the type invariant.
        unsafe { (*self.header()).size = 0 };
    }

    pub fn count(&self) -> Size {
        // SAFETY: header is valid per the type invariant.
        unsafe { (*self.header()).size }
    }

    #[inline]
    fn header(&self) -> *mut Header {
        self.metadata.cast::<Header>().wrapping_sub(1)
    }

    #[inline]
    fn set_size(&mut self, size: Size) {
        // SAFETY: header is valid per the type invariant.
        unsafe { (*self.header()).size = size };
    }

    #[inline]
    fn md(&self, idx: usize) -> *mut Metadata {
        debug_assert!(idx < self.cap as usize);
        // SAFETY: idx < cap, inside the metadata array.
        unsafe { self.metadata.add(idx) }
    }

    #[inline]
    fn key_at(&self, idx: usize) -> *mut K {
        debug_assert!(idx < self.cap as usize);
        // SAFETY: idx < cap, inside the keys array.
        unsafe { self.keys.add(idx) }
    }

    #[inline]
    fn value_at(&self, idx: usize) -> *mut V {
        debug_assert!(idx < self.cap as usize);
        // SAFETY: idx < cap, inside the values array.
        unsafe { self.values.add(idx) }
    }

    pub fn capacity(&self) -> Size {
        self.cap
    }

    /// Maximum number of entries the map will hold. This is less than
    /// capacity when max_load_percentage is below 100, which keeps free
    /// slots in every probe chain and bounds probe lengths.
    pub fn max_load(&self) -> Size {
        Self::max_load_for_capacity(self.cap)
    }

    pub fn iterator(&self) -> MapIterator<'_, K, V, C, MAX_LOAD_PERCENTAGE> {
        MapIterator { hm: self, index: 0 }
    }

    pub fn key_iterator(&self) -> KeyIterator<K> {
        FieldIterator {
            len: self.cap as usize,
            metadata: self.metadata,
            items: self.keys,
        }
    }

    pub fn value_iterator(&self) -> ValueIterator<V> {
        FieldIterator {
            len: self.cap as usize,
            metadata: self.metadata,
            items: self.values,
        }
    }

    /// Insert an entry in the map. Assumes it is not already present.
    pub fn put_no_clobber_context(&mut self, key: K, value: V, ctx: &C) -> Result<(), AllocError> {
        debug_assert!(!self.contains_context(key, ctx));
        self.check_capacity(1)?;

        self.put_assume_capacity_no_clobber_context(key, value, ctx);
        Ok(())
    }

    /// Asserts there is enough capacity to store the new key-value pair.
    /// Clobbers any existing data. To detect if a put would clobber
    /// existing data, see `getOrPutAssumeCapacity`.
    pub fn put_assume_capacity_context(&mut self, key: K, value: V, ctx: &C) {
        let gop = self.get_or_put_assume_capacity_context(key, ctx);
        // SAFETY: value_ptr points at a slot of this map.
        unsafe { gop.value_ptr.write(value) };
    }

    /// Insert an entry in the map. Assumes it is not already present,
    /// and that no allocation is needed.
    pub fn put_assume_capacity_no_clobber_context(&mut self, key: K, value: V, ctx: &C) {
        debug_assert!(!self.contains_context(key, ctx));

        // A free slot must exist for the probe below to terminate.
        assert!(self.count() < self.cap);

        let hash = ctx.hash(&key);
        let mask = (self.cap - 1) as usize;
        let mut idx = (hash as usize) & mask;

        // SAFETY (all derefs below): indices are masked into [0, cap).
        unsafe {
            while (*self.md(idx)).is_used() {
                idx = (idx + 1) & mask;
            }

            (*self.md(idx)).fill(Metadata::take_fingerprint(hash));
            self.key_at(idx).write(key);
            self.value_at(idx).write(value);
        }
        self.set_size(self.count() + 1);
    }

    /// Inserts a new `Entry` into the hash map, returning the previous one, if any.
    pub fn fetch_put_context(&mut self, key: K, value: V, ctx: &C) -> Result<Option<KV<K, V>>, AllocError> {
        let gop = self.get_or_put_context(key, ctx)?;
        let mut result: Option<KV<K, V>> = None;
        // SAFETY: gop pointers point at a slot of this map.
        unsafe {
            if gop.found_existing {
                result = Some(KV {
                    key: *gop.key_ptr,
                    value: *gop.value_ptr,
                });
            }
            gop.value_ptr.write(value);
        }
        Ok(result)
    }

    /// Inserts a new `Entry` into the hash map, returning the previous one, if any.
    /// If insertion happens, asserts there is enough capacity without allocating.
    pub fn fetch_put_assume_capacity_context(&mut self, key: K, value: V, ctx: &C) -> Option<KV<K, V>> {
        let gop = self.get_or_put_assume_capacity_context(key, ctx);
        let mut result: Option<KV<K, V>> = None;
        // SAFETY: gop pointers point at a slot of this map.
        unsafe {
            if gop.found_existing {
                result = Some(KV {
                    key: *gop.key_ptr,
                    value: *gop.value_ptr,
                });
            }
            gop.value_ptr.write(value);
        }
        result
    }

    /// If there is an `Entry` with a matching key, it is deleted from
    /// the hash map, and then returned from this function. Removal may
    /// move other entries: any previously returned key or value
    /// pointers are invalidated.
    pub fn fetch_remove_context(&mut self, key: K, ctx: &C) -> Option<KV<K, V>> {
        let idx = self.get_index(&key, ctx)?;
        // SAFETY: idx is a used slot.
        let result = unsafe {
            KV {
                key: *self.key_at(idx),
                value: *self.value_at(idx),
            }
        };
        self.remove_by_index_context(idx, ctx);
        Some(result)
    }

    /// Find the index containing the data for the given key.
    #[inline(always)]
    fn get_index<Q: ?Sized, A: HashContext<K, Q>>(&self, key: &Q, ctx: &A) -> Option<usize> {
        if self.count() == 0 {
            return None;
        }

        let hash: Hash = ctx.hash(key);
        let mask = (self.cap - 1) as usize;
        let fingerprint = Metadata::take_fingerprint(hash);
        // Don't loop indefinitely when there are no free slots.
        let mut limit = self.cap;
        let mut idx = (hash as usize) & mask;

        // SAFETY (all derefs below): indices are masked into [0, cap) and
        // keys of used slots are initialized.
        unsafe {
            let mut metadata = *self.md(idx);
            while !metadata.is_free() && limit != 0 {
                if metadata.is_used() && metadata.fingerprint() == fingerprint {
                    let test_key = &*self.key_at(idx);
                    if ctx.eql(key, test_key) {
                        return Some(idx);
                    }
                }

                limit -= 1;
                idx = (idx + 1) & mask;
                metadata = *self.md(idx);
            }
        }

        None
    }

    pub fn get_entry_context(&self, key: K, ctx: &C) -> Option<Entry<K, V>> {
        self.get_entry_adapted(&key, ctx)
    }
    pub fn get_entry_adapted<Q: ?Sized, A: HashContext<K, Q>>(&self, key: &Q, ctx: &A) -> Option<Entry<K, V>> {
        self.get_index(key, ctx).map(|idx| Entry {
            key_ptr: self.key_at(idx),
            value_ptr: self.value_at(idx),
        })
    }

    /// Insert an entry if the associated key is not already present, otherwise update preexisting value.
    pub fn put_context(&mut self, key: K, value: V, ctx: &C) -> Result<(), AllocError> {
        let result = self.get_or_put_context(key, ctx)?;
        // SAFETY: value_ptr points at a slot of this map.
        unsafe { result.value_ptr.write(value) };
        Ok(())
    }

    /// Get an optional pointer to the actual key associated with adapted key, if present.
    pub fn get_key_ptr_context(&self, key: K, ctx: &C) -> Option<*mut K> {
        self.get_key_ptr_adapted(&key, ctx)
    }
    pub fn get_key_ptr_adapted<Q: ?Sized, A: HashContext<K, Q>>(&self, key: &Q, ctx: &A) -> Option<*mut K> {
        self.get_index(key, ctx).map(|idx| self.key_at(idx))
    }

    /// Get a copy of the actual key associated with adapted key, if present.
    pub fn get_key_context(&self, key: K, ctx: &C) -> Option<K> {
        self.get_key_adapted(&key, ctx)
    }
    pub fn get_key_adapted<Q: ?Sized, A: HashContext<K, Q>>(&self, key: &Q, ctx: &A) -> Option<K> {
        // SAFETY: idx is a used slot.
        self.get_index(key, ctx).map(|idx| unsafe { *self.key_at(idx) })
    }

    /// Get an optional pointer to the value associated with key, if present.
    pub fn get_ptr_context(&self, key: K, ctx: &C) -> Option<*mut V> {
        self.get_ptr_adapted(&key, ctx)
    }
    pub fn get_ptr_adapted<Q: ?Sized, A: HashContext<K, Q>>(&self, key: &Q, ctx: &A) -> Option<*mut V> {
        self.get_index(key, ctx).map(|idx| self.value_at(idx))
    }

    /// Get a copy of the value associated with key, if present.
    pub fn get_context(&self, key: K, ctx: &C) -> Option<V> {
        self.get_adapted(&key, ctx)
    }
    pub fn get_adapted<Q: ?Sized, A: HashContext<K, Q>>(&self, key: &Q, ctx: &A) -> Option<V> {
        // SAFETY: idx is a used slot.
        self.get_index(key, ctx).map(|idx| unsafe { *self.value_at(idx) })
    }

    pub fn get_or_put_context(&mut self, key: K, ctx: &C) -> Result<GetOrPutResult<K, V>, AllocError> {
        let gop = self.get_or_put_context_adapted(&key, ctx)?;
        if !gop.found_existing {
            // SAFETY: key_ptr points at a slot of this map.
            unsafe { gop.key_ptr.write(key) };
        }
        Ok(gop)
    }

    pub fn get_or_put_context_adapted<Q: ?Sized, A: HashContext<K, Q>>(
        &mut self,
        key: &Q,
        key_ctx: &A,
    ) -> Result<GetOrPutResult<K, V>, AllocError> {
        if let Err(err) = self.check_capacity(1) {
            // The map is full. Try to do the lookup anyway; if we find
            // an existing item, we can return it. Otherwise return the
            // error, we could not add another.
            let index = self.get_index(key, key_ctx).ok_or(err)?;
            return Ok(GetOrPutResult {
                key_ptr: self.key_at(index),
                value_ptr: self.value_at(index),
                found_existing: true,
            });
        }
        Ok(self.get_or_put_assume_capacity_adapted(key, key_ctx))
    }

    pub fn get_or_put_assume_capacity_context(&mut self, key: K, ctx: &C) -> GetOrPutResult<K, V> {
        let result = self.get_or_put_assume_capacity_adapted(&key, ctx);
        if !result.found_existing {
            // SAFETY: key_ptr points at a slot of this map.
            unsafe { result.key_ptr.write(key) };
        }
        result
    }

    pub fn get_or_put_assume_capacity_adapted<Q: ?Sized, A: HashContext<K, Q>>(
        &mut self,
        key: &Q,
        ctx: &A,
    ) -> GetOrPutResult<K, V> {
        let hash: Hash = ctx.hash(key);
        let mask = (self.cap - 1) as usize;
        let fingerprint = Metadata::take_fingerprint(hash);
        let mut limit = self.cap;
        let mut idx = (hash as usize) & mask;

        // SAFETY (all derefs below): indices are masked into [0, cap) and
        // keys of used slots are initialized.
        unsafe {
            let mut metadata = self.md(idx);
            while !(*metadata).is_free() && limit != 0 {
                if (*metadata).is_used() && (*metadata).fingerprint() == fingerprint {
                    let test_key = self.key_at(idx);
                    if ctx.eql(key, &*test_key) {
                        return GetOrPutResult {
                            key_ptr: test_key,
                            value_ptr: self.value_at(idx),
                            found_existing: true,
                        };
                    }
                }

                limit -= 1;
                idx = (idx + 1) & mask;
                metadata = self.md(idx);
            }

            // The caller guaranteed capacity for at least one new entry, so
            // the probe must have ended at a free slot. Anything else means
            // the assume-capacity contract was violated and we would be
            // silently overwriting a live entry.
            assert!((*metadata).is_free());

            (*metadata).fill(fingerprint);
        }
        // The new key and value slots are left as-is ("undefined"); the
        // caller writes them through the returned pointers.
        let new_key = self.key_at(idx);
        let new_value = self.value_at(idx);
        self.set_size(self.count() + 1);

        GetOrPutResult {
            key_ptr: new_key,
            value_ptr: new_value,
            found_existing: false,
        }
    }

    pub fn get_or_put_value_context(&mut self, key: K, value: V, ctx: &C) -> Result<Entry<K, V>, AllocError> {
        let res = self.get_or_put_context_adapted(&key, ctx)?;
        if !res.found_existing {
            // SAFETY: pointers point at a slot of this map.
            unsafe {
                res.key_ptr.write(key);
                res.value_ptr.write(value);
            }
        }
        Ok(Entry {
            key_ptr: res.key_ptr,
            value_ptr: res.value_ptr,
        })
    }

    /// Return true if there is a value associated with key in the map.
    pub fn contains_context(&self, key: K, ctx: &C) -> bool {
        self.contains_adapted(&key, ctx)
    }
    pub fn contains_adapted<Q: ?Sized, A: HashContext<K, Q>>(&self, key: &Q, ctx: &A) -> bool {
        self.get_index(key, ctx).is_some()
    }

    /// Remove the entry at the given index using backward-shift deletion
    /// (Knuth vol. 3, section 6.4, algorithm R): rather than marking the
    /// slot with a tombstone, restore the table to the state it would be
    /// in had the removed key never been inserted. Any entry whose probe
    /// sequence passes over the hole is moved into it, which moves the
    /// hole further along the cluster, until the cluster ends at a free
    /// slot.
    fn remove_by_index_context(&mut self, idx: usize, ctx: &C) {
        let mask: usize = (self.cap - 1) as usize;

        // A completely full table has no free slot to terminate the
        // scan, so bound it to one full cycle. That is sufficient: the
        // hole only ever moves forward to slots the scan has already
        // visited, so each entry needs to be considered exactly once.
        let mut hole = idx;
        let mut j = idx;
        let mut limit = self.cap - 1;
        // SAFETY (all derefs below): indices are masked into [0, cap) and
        // keys/values of used slots are initialized.
        unsafe {
            while limit != 0 {
                j = (j + 1) & mask;
                if (*self.md(j)).is_free() {
                    break;
                }

                // The entry at `j` may move into the hole only if the hole
                // lies on its probe path, i.e. cyclically within [home, j).
                // Otherwise the move would place it before its home slot
                // and lookups could no longer find it.
                let home: usize = (ctx.hash(&*self.key_at(j)) as usize) & mask;
                if (hole.wrapping_sub(home) & mask) < (j.wrapping_sub(home) & mask) {
                    *self.md(hole) = *self.md(j);
                    self.key_at(hole).write(*self.key_at(j));
                    self.value_at(hole).write(*self.value_at(j));
                    hole = j;
                }
                limit -= 1;
            }

            *self.md(hole) = Metadata::default();
        }
        // keys_ptr[hole] / values_ptr[hole] = undefined: left as-is.
        self.set_size(self.count() - 1);
    }

    pub fn remove_context(&mut self, key: K, ctx: &C) -> bool {
        match self.get_index(&key, ctx) {
            Some(idx) => {
                self.remove_by_index_context(idx, ctx);
                true
            }
            None => false,
        }
    }

    /// Delete the entry with key pointed to by key_ptr from the hash map.
    /// key_ptr is assumed to be a valid pointer to a key that is present
    /// in the hash map. Removal may move other entries: any previously
    /// returned key or value pointers are invalidated.
    pub fn remove_by_ptr_context(&mut self, key_ptr: *mut K, ctx: &C) {
        // if @sizeOf(K) == 0 then there is at most one item in the hash
        // map, which is assumed to exist as key_ptr must be valid.  This
        // item must be at index 0.
        let idx = if core::mem::size_of::<K>() > 0 {
            (key_ptr as usize - self.keys as usize) / core::mem::size_of::<K>()
        } else {
            0
        };

        self.remove_by_index_context(idx, ctx);
    }

    fn init_metadatas(&mut self) {
        // SAFETY: the metadata array holds `cap` bytes (type invariant).
        unsafe {
            core::ptr::write_bytes(self.metadata.cast::<u8>(), 0, core::mem::size_of::<Metadata>() * self.cap as usize)
        };
    }

    /// Returns an error if the map cannot hold `new_count` more entries.
    /// This map is fixed-capacity so nothing can be done to make room;
    /// the caller must grow the backing memory and rebuild the map.
    fn check_capacity(&self, new_count: Size) -> Result<(), AllocError> {
        let available = self.max_load() - self.count();
        if new_count > available {
            return Err(AllocError::OutOfMemory);
        }
        Ok(())
    }

    fn max_load_for_capacity(cap: Size) -> Size {
        if cap == 0 {
            return 0;
        }
        ((cap as u64 * MAX_LOAD_PERCENTAGE as u64) / 100) as Size
    }

    /// Returns the memory layout for the buffer for a given capacity.
    /// The actual size may be able to fit more than the given capacity
    /// because capacity is rounded up to the next power of two. This is
    /// a design requirement for this hash map implementation.
    pub fn layout_for_capacity(new_capacity: Size) -> Layout {
        assert!(new_capacity == 0 || new_capacity.is_power_of_two());

        // Cast to usize to prevent overflow in size calculations.
        // See: https://github.com/ziglang/zig/pull/19048
        let cap: usize = new_capacity as usize;

        // Pack our header, metadata, keys, and values.
        let meta_start = core::mem::size_of::<Header>();
        let meta_end = meta_start + cap * core::mem::size_of::<Metadata>();
        let keys_start = align_forward(meta_end, Self::KEY_ALIGN);
        let keys_end = keys_start + cap * core::mem::size_of::<K>();
        let vals_start = align_forward(keys_end, Self::VAL_ALIGN);
        let vals_end = vals_start + cap * core::mem::size_of::<V>();

        // Our total memory size required is the end of our values
        // aligned to the base required alignment.
        let total_size = align_forward(vals_end, Self::BASE_ALIGN);

        Layout {
            total_size,
            metadata_start: meta_start,
            keys_start,
            vals_start,
            capacity: new_capacity,
        }
    }

    /// Returns a layout with enough raw slots to hold `new_size` entries
    /// at the configured maximum load factor.
    pub fn layout_for_size(new_size: Size) -> Layout {
        if new_size == 0 {
            return Self::layout_for_capacity(0);
        }

        // Scale the requested number of entries up to the raw slot count
        // required by the load factor. Widen first so `new_size * 100`
        // cannot overflow Size.
        let minimum_capacity: u64 = (new_size as u64 * 100).div_ceil(MAX_LOAD_PERCENTAGE as u64);

        // Capacities must be powers of two, so the largest capacity that
        // fits in Size is the highest bit rather than maxInt(Size).
        let max_capacity: u64 = 1u64 << (Size::BITS - 1);
        if minimum_capacity > max_capacity {
            return Self::layout_for_capacity(max_capacity as Size);
        }

        // Linear probing uses a mask for wraparound, which requires the
        // final raw capacity to be rounded up to a power of two.
        let raw_capacity = minimum_capacity.next_power_of_two();
        Self::layout_for_capacity(raw_capacity as Size)
    }
}

/// The context-less API. Zig requires a zero-sized context for these and
/// passes `undefined`; here the context is `C::default()`.
impl<K: Copy, V: Copy, C: HashContext<K> + Default, const P: u8> HashMapUnmanaged<K, V, C, P> {
    /// Insert an entry in the map. Assumes it is not already present.
    pub fn put_no_clobber(&mut self, key: K, value: V) -> Result<(), AllocError> {
        self.put_no_clobber_context(key, value, &C::default())
    }

    /// Asserts there is enough capacity to store the new key-value pair.
    /// Clobbers any existing data.
    pub fn put_assume_capacity(&mut self, key: K, value: V) {
        self.put_assume_capacity_context(key, value, &C::default())
    }

    /// Insert an entry in the map. Assumes it is not already present,
    /// and that no allocation is needed.
    pub fn put_assume_capacity_no_clobber(&mut self, key: K, value: V) {
        self.put_assume_capacity_no_clobber_context(key, value, &C::default())
    }

    /// Inserts a new `Entry` into the hash map, returning the previous one, if any.
    pub fn fetch_put(&mut self, key: K, value: V) -> Result<Option<KV<K, V>>, AllocError> {
        self.fetch_put_context(key, value, &C::default())
    }

    /// Inserts a new `Entry` into the hash map, returning the previous one, if any.
    /// If insertion happens, asserts there is enough capacity without allocating.
    pub fn fetch_put_assume_capacity(&mut self, key: K, value: V) -> Option<KV<K, V>> {
        self.fetch_put_assume_capacity_context(key, value, &C::default())
    }

    /// If there is an `Entry` with a matching key, it is deleted from
    /// the hash map, and then returned from this function.
    pub fn fetch_remove(&mut self, key: K) -> Option<KV<K, V>> {
        self.fetch_remove_context(key, &C::default())
    }

    pub fn get_entry(&self, key: K) -> Option<Entry<K, V>> {
        self.get_entry_context(key, &C::default())
    }

    /// Insert an entry if the associated key is not already present, otherwise update preexisting value.
    pub fn put(&mut self, key: K, value: V) -> Result<(), AllocError> {
        self.put_context(key, value, &C::default())
    }

    /// Get an optional pointer to the actual key associated with adapted key, if present.
    pub fn get_key_ptr(&self, key: K) -> Option<*mut K> {
        self.get_key_ptr_context(key, &C::default())
    }

    /// Get a copy of the actual key associated with adapted key, if present.
    pub fn get_key(&self, key: K) -> Option<K> {
        self.get_key_context(key, &C::default())
    }

    /// Get an optional pointer to the value associated with key, if present.
    pub fn get_ptr(&self, key: K) -> Option<*mut V> {
        self.get_ptr_context(key, &C::default())
    }

    /// Get a copy of the value associated with key, if present.
    pub fn get(&self, key: K) -> Option<V> {
        self.get_context(key, &C::default())
    }

    pub fn get_or_put(&mut self, key: K) -> Result<GetOrPutResult<K, V>, AllocError> {
        self.get_or_put_context(key, &C::default())
    }

    pub fn get_or_put_adapted<Q: ?Sized, A: HashContext<K, Q>>(
        &mut self,
        key: &Q,
        key_ctx: &A,
    ) -> Result<GetOrPutResult<K, V>, AllocError> {
        self.get_or_put_context_adapted(key, key_ctx)
    }

    pub fn get_or_put_assume_capacity(&mut self, key: K) -> GetOrPutResult<K, V> {
        self.get_or_put_assume_capacity_context(key, &C::default())
    }

    pub fn get_or_put_value(&mut self, key: K, value: V) -> Result<Entry<K, V>, AllocError> {
        self.get_or_put_value_context(key, value, &C::default())
    }

    /// Return true if there is a value associated with key in the map.
    pub fn contains(&self, key: K) -> bool {
        self.contains_context(key, &C::default())
    }

    /// If there is an `Entry` with a matching key, it is deleted from
    /// the hash map, and this function returns true.  Otherwise this
    /// function returns false. Removal may move other entries: any
    /// previously returned key or value pointers are invalidated.
    pub fn remove(&mut self, key: K) -> bool {
        self.remove_context(key, &C::default())
    }

    /// Delete the entry with key pointed to by key_ptr from the hash map.
    pub fn remove_by_ptr(&mut self, key_ptr: *mut K) {
        self.remove_by_ptr_context(key_ptr, &C::default())
    }
}

/// Port of Zig's `std.hash.Wyhash` (the final v4.2 variant used by the Zig
/// standard library). Produces the same values as Zig for the same input
/// and seed.
#[derive(Clone)]
pub struct Wyhash {
    a: u64,
    b: u64,
    state: [u64; 3],
    total_len: usize,

    buf: [u8; 48],
    buf_len: usize,
}

impl Wyhash {
    const SECRET: [u64; 4] = [
        0xa0761d6478bd642f,
        0xe7037ed1a0b428db,
        0x8ebc6af09c88c6e3,
        0x589965cc75374cc3,
    ];

    pub fn init(seed: u64) -> Wyhash {
        let s0 = seed ^ Self::mix(seed ^ Self::SECRET[0], Self::SECRET[1]);
        Wyhash {
            a: 0,
            b: 0,
            state: [s0, s0, s0],
            total_len: 0,
            buf: [0; 48],
            buf_len: 0,
        }
    }

    // This is subtly different from other hash function update calls. Wyhash requires the last
    // full 48-byte block to be run through final1 if is exactly aligned to 48-bytes.
    pub fn update(&mut self, input: &[u8]) {
        self.total_len += input.len();

        if input.len() <= 48 - self.buf_len {
            self.buf[self.buf_len..][..input.len()].copy_from_slice(input);
            self.buf_len += input.len();
            return;
        }

        let mut i: usize = 0;

        if self.buf_len > 0 {
            i = 48 - self.buf_len;
            self.buf[self.buf_len..][..i].copy_from_slice(&input[..i]);
            let block = self.buf;
            self.round(&block);
            self.buf_len = 0;
        }

        while i + 48 < input.len() {
            self.round(input[i..][..48].try_into().unwrap());
            i += 48;
        }

        let remaining_bytes = &input[i..];
        if remaining_bytes.len() < 16 && i >= 48 {
            let rem = 16 - remaining_bytes.len();
            let len = self.buf.len();
            self.buf[len - rem..].copy_from_slice(&input[i - rem..i]);
        }
        self.buf[..remaining_bytes.len()].copy_from_slice(remaining_bytes);
        self.buf_len = remaining_bytes.len();
    }

    pub fn final_(&self) -> u64 {
        let mut input: &[u8] = &self.buf[..self.buf_len];
        // ensure idempotency
        let mut new_self = self.shallow_copy();
        let mut scratch = [0u8; 16];

        if self.total_len <= 16 {
            new_self.small_key(input);
        } else {
            let mut offset: usize = 0;
            if self.buf_len < 16 {
                let rem = 16 - self.buf_len;
                scratch[..rem].copy_from_slice(&self.buf[self.buf.len() - rem..][..rem]);
                scratch[rem..][..self.buf_len].copy_from_slice(&self.buf[..self.buf_len]);

                // Same as input but with additional bytes preceding start in case of a short buffer
                input = &scratch;
                offset = rem;
            }

            new_self.final0();
            new_self.final1(input, offset);
        }

        new_self.final2()
    }

    // Copies the core wyhash state but not any internal buffers.
    fn shallow_copy(&self) -> Wyhash {
        Wyhash {
            a: self.a,
            b: self.b,
            state: self.state,
            total_len: self.total_len,
            buf: [0; 48],
            buf_len: 0,
        }
    }

    #[inline(always)]
    fn small_key(&mut self, input: &[u8]) {
        debug_assert!(input.len() <= 16);

        if input.len() >= 4 {
            let end = input.len() - 4;
            let quarter = (input.len() >> 3) << 2;
            self.a = (Self::read4(&input[0..]) << 32) | Self::read4(&input[quarter..]);
            self.b = (Self::read4(&input[end..]) << 32) | Self::read4(&input[end - quarter..]);
        } else if !input.is_empty() {
            self.a = ((input[0] as u64) << 16) | ((input[input.len() >> 1] as u64) << 8) | input[input.len() - 1] as u64;
            self.b = 0;
        } else {
            self.a = 0;
            self.b = 0;
        }
    }

    #[inline(always)]
    fn round(&mut self, input: &[u8; 48]) {
        for i in 0..3 {
            let a = Self::read8(&input[8 * (2 * i)..]);
            let b = Self::read8(&input[8 * (2 * i + 1)..]);
            self.state[i] = Self::mix(a ^ Self::SECRET[i + 1], b ^ self.state[i]);
        }
    }

    #[inline(always)]
    fn read4(data: &[u8]) -> u64 {
        u32::from_le_bytes(data[..4].try_into().unwrap()) as u64
    }

    #[inline(always)]
    fn read8(data: &[u8]) -> u64 {
        u64::from_le_bytes(data[..8].try_into().unwrap())
    }

    #[inline(always)]
    fn mum(a: &mut u64, b: &mut u64) {
        let x = (*a as u128).wrapping_mul(*b as u128);
        *a = x as u64;
        *b = (x >> 64) as u64;
    }

    #[inline(always)]
    fn mix(a_: u64, b_: u64) -> u64 {
        let mut a = a_;
        let mut b = b_;
        Self::mum(&mut a, &mut b);
        a ^ b
    }

    #[inline(always)]
    fn final0(&mut self) {
        self.state[0] ^= self.state[1] ^ self.state[2];
    }

    // input_lb must be at least 16-bytes long (in shorter key cases the smallKey function will be
    // used instead). We use an index into a slice to for comptime processing as opposed to if we
    // used pointers.
    #[inline(always)]
    fn final1(&mut self, input_lb: &[u8], start_pos: usize) {
        debug_assert!(input_lb.len() >= 16);
        debug_assert!(input_lb.len() - start_pos <= 48);
        let input = &input_lb[start_pos..];

        let mut i: usize = 0;
        while i + 16 < input.len() {
            self.state[0] = Self::mix(
                Self::read8(&input[i..]) ^ Self::SECRET[1],
                Self::read8(&input[i + 8..]) ^ self.state[0],
            );
            i += 16;
        }

        self.a = Self::read8(&input_lb[input_lb.len() - 16..]);
        self.b = Self::read8(&input_lb[input_lb.len() - 8..]);
    }

    #[inline(always)]
    fn final2(&mut self) -> u64 {
        self.a ^= Self::SECRET[1];
        self.b ^= self.state[0];
        Self::mum(&mut self.a, &mut self.b);
        Self::mix(self.a ^ Self::SECRET[0] ^ self.total_len as u64, self.b ^ Self::SECRET[1])
    }

    /// One-shot hash of `input`.
    #[inline(always)]
    pub fn hash(seed: u64, input: &[u8]) -> u64 {
        let mut this = Wyhash::init(seed);

        if input.len() <= 16 {
            this.small_key(input);
        } else {
            let mut i: usize = 0;
            if input.len() >= 48 {
                while i + 48 < input.len() {
                    this.round(input[i..][..48].try_into().unwrap());
                    i += 48;
                }
                this.final0();
            }
            this.final1(input, i);
        }

        this.total_len = input.len();
        this.final2()
    }
}

impl core::hash::Hasher for Wyhash {
    fn finish(&self) -> u64 {
        self.final_()
    }

    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::size::AlignedBuf;

    /// Verify the canonical placement invariant that backward-shift deletion
    /// maintains: every used entry is reachable from its home slot without
    /// crossing a free slot. This is exactly the property lookups depend on.
    fn expect_canonical<K: Copy, V: Copy, C: HashContext<K>, const P: u8>(
        map: &HashMapUnmanaged<K, V, C, P>,
        ctx: &C,
    ) {
        let cap = map.capacity() as usize;
        let mask = cap - 1;
        let mut used: usize = 0;
        for idx in 0..cap {
            let metadata = unsafe { *map.metadata.add(idx) };
            if !metadata.is_used() {
                continue;
            }
            used += 1;

            let mut probe: usize = (ctx.hash(unsafe { &*map.keys.add(idx) }) as usize) & mask;
            while probe != idx {
                assert!(unsafe { *map.metadata.add(probe) }.is_used());
                probe = (probe + 1) & mask;
            }
        }
        assert_eq!(map.count() as usize, used);
    }

    /// Allocate a buffer and init an unmanaged map over it (Zig:
    /// `alignedAlloc` + `Map.init(.init(buf), layout)`).
    fn new_map<K: Copy, V: Copy, C: HashContext<K>, const P: u8>(
        cap: Size,
    ) -> (AlignedBuf, HashMapUnmanaged<K, V, C, P>) {
        let layout = HashMapUnmanaged::<K, V, C, P>::layout_for_capacity(cap);
        let buf = AlignedBuf::new(layout.total_size, HashMapUnmanaged::<K, V, C, P>::BASE_ALIGN);
        // SAFETY: buf has total_size bytes aligned to BASE_ALIGN and outlives the map.
        let map = unsafe { HashMapUnmanaged::init(OffsetBuf::init(buf.ptr()), layout) };
        (buf, map)
    }

    /// Small xoshiro256++ PRNG for the randomized tests (Zig:
    /// `std.Random.DefaultPrng`). Not bit-identical to Zig's sequence;
    /// only the statistical role matters.
    struct Prng {
        s: [u64; 4],
    }

    impl Prng {
        fn init(seed: u64) -> Self {
            let mut x = seed;
            let mut next = || {
                x = x.wrapping_add(0x9e3779b97f4a7c15);
                let mut z = x;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
                z ^ (z >> 31)
            };
            Prng {
                s: [next(), next(), next(), next()],
            }
        }

        fn next(&mut self) -> u64 {
            let r = self.s[0].wrapping_add(self.s[3]).rotate_left(23).wrapping_add(self.s[0]);
            let t = self.s[1] << 17;
            self.s[2] ^= self.s[0];
            self.s[3] ^= self.s[1];
            self.s[1] ^= self.s[2];
            self.s[0] ^= self.s[3];
            self.s[2] ^= t;
            self.s[3] = self.s[3].rotate_left(45);
            r
        }

        fn uint_less_than(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        fn shuffle<T>(&mut self, items: &mut [T]) {
            if items.len() < 2 {
                return;
            }
            for i in 0..items.len() - 1 {
                let j = i + self.uint_less_than((items.len() - i) as u64) as usize;
                items.swap(i, j);
            }
        }
    }

    type M<K, V> = AutoHashMapUnmanaged<K, V, DEFAULT_MAX_LOAD_PERCENTAGE>;

    #[test]
    fn wyhash_test_vectors() {
        let hash = Wyhash::hash;
        assert_eq!(hash(0, b""), 0x409638ee2bde459);
        assert_eq!(hash(1, b"a"), 0xa8412d091b5fe0a9);
        assert_eq!(hash(2, b"abc"), 0x32dd92e4b2915153);
        assert_eq!(hash(3, b"message digest"), 0x8619124089a3a16b);
        assert_eq!(hash(4, b"abcdefghijklmnopqrstuvwxyz"), 0x7a43afb61d7f5f40);
        assert_eq!(
            hash(5, b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"),
            0xff42329b90e50d58
        );
        assert_eq!(
            hash(
                6,
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            0xc39cab13b115aad3
        );
    }

    #[test]
    fn wyhash_iterative_matches_oneshot() {
        let data: Vec<u8> = (0..200u32).map(|i| (i * 7 + 3) as u8).collect();
        for len in 0..data.len() {
            for split in [0, 1, 7, 16, 47, 48, 49, 100] {
                let split = split.min(len);
                let mut h = Wyhash::init(42);
                h.update(&data[..split]);
                h.update(&data[split..len]);
                assert_eq!(Wyhash::hash(42, &data[..len]), h.final_(), "len={len} split={split}");
            }
        }
    }

    #[test]
    fn hash_map_basic_usage() {
        let (_buf, mut map) = new_map::<u32, u32, AutoContext<u32>, 100>(16);

        let count = 5;
        let mut total: u32 = 0;
        for i in 0..count {
            map.put(i, i).unwrap();
            total += i;
        }

        let mut sum: u32 = 0;
        for kv in map.iterator() {
            sum += unsafe { *kv.key_ptr };
        }
        assert_eq!(total, sum);

        sum = 0;
        for i in 0..count {
            assert_eq!(i, map.get(i).unwrap());
            sum += map.get(i).unwrap();
        }
        assert_eq!(total, sum);
    }

    #[test]
    fn hash_map_ensure_total_capacity() {
        let (_buf, mut map) = new_map::<i32, i32, AutoContext<i32>, 100>(32);

        let initial_capacity = map.capacity();
        assert!(initial_capacity >= 20);
        for i in 0..20 {
            assert!(map.fetch_put_assume_capacity(i, i + 10).is_none());
        }
        // shouldn't resize from putAssumeCapacity
        assert!(initial_capacity == map.capacity());
    }

    #[test]
    fn hash_map_ensure_unused_capacity_with_removals() {
        let (_buf, mut map) = new_map::<i32, i32, AutoContext<i32>, 100>(32);

        for i in 0..100 {
            map.ensure_unused_capacity(1).unwrap();
            map.put_assume_capacity(i, i);
            let _ = map.remove(i);
        }
    }

    #[test]
    fn hash_map_clear_retaining_capacity() {
        let (_buf, mut map): (_, M<u32, u32>) = new_map(16);

        map.clear_retaining_capacity();

        map.put(1, 1).unwrap();
        assert_eq!(map.get(1).unwrap(), 1);
        assert_eq!(map.count(), 1);

        map.clear_retaining_capacity();
        map.put_assume_capacity(1, 1);
        assert_eq!(map.get(1).unwrap(), 1);
        assert_eq!(map.count(), 1);

        let actual_cap = map.capacity();
        assert!(actual_cap > 0);

        map.clear_retaining_capacity();
        map.clear_retaining_capacity();
        assert_eq!(map.count(), 0);
        assert_eq!(map.capacity(), actual_cap);
        assert!(!map.contains(1));
    }

    #[test]
    fn hash_map_ensure_total_capacity_with_existing_elements() {
        let cap = 8;
        let (_buf, mut map): (_, M<u32, u32>) = new_map(cap);

        map.put(0, 0).unwrap();
        assert_eq!(map.count(), 1);
        assert_eq!(map.capacity(), cap);

        assert_eq!(Err(AllocError::OutOfMemory), map.ensure_total_capacity(65));
        assert_eq!(map.count(), 1);
        assert_eq!(map.capacity(), cap);
    }

    #[test]
    fn hash_map_remove() {
        let (_buf, mut map): (_, M<u32, u32>) = new_map(32);

        for i in 0..16 {
            map.put(i, i).unwrap();
        }

        for i in 0..16 {
            if i % 3 == 0 {
                let _ = map.remove(i);
            }
        }
        assert_eq!(map.count(), 10);
        for kv in map.iterator() {
            unsafe {
                assert_eq!(*kv.key_ptr, *kv.value_ptr);
                assert!(*kv.key_ptr % 3 != 0);
            }
        }

        for i in 0..16 {
            if i % 3 == 0 {
                assert!(!map.contains(i));
            } else {
                assert_eq!(map.get(i).unwrap(), i);
            }
        }
    }

    #[test]
    fn hash_map_reverse_removes() {
        let (_buf, mut map): (_, M<u32, u32>) = new_map(32);

        for i in 0..16 {
            map.put_no_clobber(i, i).unwrap();
        }

        let mut i = 16;
        while i > 0 {
            let _ = map.remove(i - 1);
            assert!(!map.contains(i - 1));
            for j in 0..i - 1 {
                assert_eq!(map.get(j).unwrap(), j);
            }
            i -= 1;
        }

        assert_eq!(map.count(), 0);
    }

    #[test]
    fn hash_map_multiple_removes_on_same_metadata() {
        let (_buf, mut map): (_, M<u32, u32>) = new_map(32);

        for i in 0..16 {
            map.put(i, i).unwrap();
        }

        let _ = map.remove(7);
        let _ = map.remove(15);
        let _ = map.remove(14);
        let _ = map.remove(13);
        assert!(!map.contains(7));
        assert!(!map.contains(15));
        assert!(!map.contains(14));
        assert!(!map.contains(13));

        for i in 0..13 {
            if i == 7 {
                assert!(!map.contains(i));
            } else {
                assert_eq!(map.get(i).unwrap(), i);
            }
        }

        map.put(15, 15).unwrap();
        map.put(13, 13).unwrap();
        map.put(14, 14).unwrap();
        map.put(7, 7).unwrap();
        for i in 0..16 {
            assert_eq!(map.get(i).unwrap(), i);
        }
    }

    #[test]
    fn hash_map_put_and_remove_loop_in_random_order() {
        let (_buf, mut map): (_, M<u32, u32>) = new_map(64);

        let mut keys: Vec<u32> = Vec::new();

        let size = 32;
        let iterations = 100;

        let mut i: u32 = 0;
        while i < size {
            keys.push(i);
            i += 1;
        }
        let mut prng = Prng::init(0);

        while i < iterations {
            prng.shuffle(&mut keys);

            for &key in &keys {
                map.put(key, key).unwrap();
            }
            assert_eq!(map.count(), size);

            for &key in &keys {
                let _ = map.remove(key);
            }
            assert_eq!(map.count(), 0);
            i += 1;
        }
    }

    #[test]
    fn hash_map_put() {
        let (_buf, mut map): (_, M<u32, u32>) = new_map(32);

        for i in 0..16 {
            map.put(i, i).unwrap();
        }

        for i in 0..16 {
            assert_eq!(map.get(i).unwrap(), i);
        }

        for i in 0..16 {
            map.put(i, i * 16 + 1).unwrap();
        }

        for i in 0..16 {
            assert_eq!(map.get(i).unwrap(), i * 16 + 1);
        }
    }

    #[test]
    fn hash_map_put_full_load() {
        let cap = 16usize;
        let (_buf, mut map): (_, M<usize, usize>) = new_map(cap as Size);

        for i in 0..cap {
            map.put(i, i).unwrap();
        }
        for i in 0..cap {
            assert_eq!(map.get(i).unwrap(), i);
        }

        assert_eq!(Err(AllocError::OutOfMemory), map.put(cap, cap));
    }

    #[test]
    fn hash_map_put_assume_capacity() {
        let (_buf, mut map): (_, M<u32, u32>) = new_map(32);

        for i in 0..20 {
            map.put_assume_capacity_no_clobber(i, i);
        }

        let mut sum = 0;
        for i in 0..20 {
            sum += unsafe { *map.get_ptr(i).unwrap() };
        }
        assert_eq!(sum, 190);

        for i in 0..20 {
            map.put_assume_capacity(i, 1);
        }

        sum = 0;
        for i in 0..20 {
            sum += map.get(i).unwrap();
        }
        assert_eq!(sum, 20);
    }

    #[test]
    fn hash_map_repeat_put_assume_capacity_remove() {
        let cap = 32;
        let (_buf, mut map): (_, M<u32, u32>) = new_map(cap);

        let limit = cap;

        for i in 0..limit {
            map.put_assume_capacity_no_clobber(i, i);
        }

        // Repeatedly delete/insert an entry without resizing the map.
        // Put to different keys so entries don't land in the just-freed slot.
        for i in 0..10 * limit {
            assert!(map.remove(i));
            if i % 2 == 0 {
                map.put_assume_capacity_no_clobber(limit + i, i);
            } else {
                map.put_assume_capacity(limit + i, i);
            }
        }

        for i in 9 * limit..10 * limit {
            assert_eq!(map.get(limit + i), Some(i));
        }
        assert_eq!(map.count(), limit);
    }

    #[derive(Default, Clone, Copy)]
    struct IdentityContext;

    impl HashContext<u32> for IdentityContext {
        fn hash(&self, key: &u32) -> u64 {
            *key as u64
        }

        fn eql(&self, a: &u32, b: &u32) -> bool {
            a == b
        }
    }

    #[test]
    fn hash_map_no_clobber_move_after_remove_at_max_load() {
        let (_buf, mut map) = new_map::<u32, u32, IdentityContext, 80>(16);
        let ctx = IdentityContext;

        // Fill the map to its maximum load.
        let max_load = map.max_load();
        for i in 0..max_load {
            map.put_assume_capacity_no_clobber_context(i, i, &ctx);
        }

        // Model a managed-cell move: remove the source and insert the value at
        // a destination known to be absent. This must work at maximum load for
        // any number of moves since removal genuinely frees a slot.
        for i in 0..100u32 {
            let src: u32 = i;
            let dst: u32 = i + max_load;
            assert!(map.remove_context(src, &ctx));
            map.put_assume_capacity_no_clobber_context(dst, dst, &ctx);

            assert_eq!(max_load, map.count());
            assert_eq!(dst, map.get_context(dst, &ctx).unwrap());
            expect_canonical(&map, &ctx);
        }
    }

    #[derive(Default, Clone, Copy)]
    struct ConstantContext;

    impl HashContext<u32> for ConstantContext {
        fn hash(&self, _: &u32) -> u64 {
            14
        }

        fn eql(&self, a: &u32, b: &u32) -> bool {
            a == b
        }
    }

    #[test]
    fn hash_map_removal_keeps_colliding_clusters_findable() {
        // All keys hash to the same home slot near the end of the table so
        // that clusters wrap around the index mask. This exercises the cyclic
        // arithmetic in backward-shift deletion.
        let cap = 16usize;
        let (_buf, mut map) = new_map::<u32, u32, ConstantContext, DEFAULT_MAX_LOAD_PERCENTAGE>(cap as Size);
        let ctx = ConstantContext;

        // Fill half the table: the cluster spans the wraparound point.
        for i in 0..cap / 2 {
            map.put_assume_capacity_no_clobber_context(i as u32, i as u32, &ctx);
        }

        // Remove from the middle of the cluster and verify all remaining
        // entries stay findable after every removal.
        let mut removed: usize = 0;
        for key in [3u32, 0, 7, 4, 1, 6, 2, 5] {
            assert!(map.remove_context(key, &ctx));
            removed += 1;

            for i in 0..cap / 2 {
                let k = i as u32;
                let v = map.get_context(k, &ctx);
                if map.contains_context(k, &ctx) {
                    assert_eq!(k, v.unwrap());
                }
            }
            assert_eq!((cap / 2 - removed) as Size, map.count());
            expect_canonical(&map, &ctx);
        }
    }

    #[test]
    fn hash_map_removal_from_a_completely_full_table() {
        let cap = 64u32;
        let (_buf, mut map): (_, M<u32, u32>) = new_map(cap);

        // A 100% load factor allows filling every raw slot, so removal cannot
        // rely on a free slot to terminate its cluster scan.
        for i in 0..cap {
            map.put_assume_capacity_no_clobber(i, i);
        }
        assert_eq!(cap, map.count());

        // Remove every other key, verifying everything else stays findable.
        let mut expected = cap;
        for i in 0..cap {
            if i % 2 != 0 {
                continue;
            }
            assert!(map.remove(i));
            expected -= 1;
            assert_eq!(expected, map.count());
        }

        for i in 0..cap {
            if i % 2 == 0 {
                assert_eq!(None, map.get(i));
            } else {
                assert_eq!(i, map.get(i).unwrap());
            }
        }
        expect_canonical(&map, &AutoContext::<u32>::default());
    }

    #[test]
    fn hash_map_random_operations_against_an_oracle() {
        let cap = 64u32;
        let (_buf, mut map): (_, M<u32, u32>) = new_map(cap);

        let mut oracle: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

        let mut prng = Prng::init(0xdeadbeef);

        // A small key space forces frequent hits, misses, and re-insertions
        // at every load factor from empty to completely full.
        let key_space = cap + cap / 2;
        for _ in 0..20_000 {
            let key = prng.uint_less_than(key_space as u64) as u32;
            match prng.uint_less_than(4) {
                0 | 1 => {
                    let value = prng.next() as u32;
                    if map.put(key, value).is_ok() {
                        oracle.insert(key, value);
                    } else {
                        // Map is full: the oracle must not know this key
                        // (put on an existing key always succeeds).
                        assert!(!oracle.contains_key(&key));
                        assert_eq!(map.count(), map.capacity());
                    }
                }
                2 => assert_eq!(oracle.remove(&key).is_some(), map.remove(key)),
                3 => assert_eq!(oracle.get(&key).copied(), map.get(key)),
                _ => unreachable!(),
            }

            assert_eq!(oracle.len(), map.count() as usize);
        }

        // Final full comparison plus the canonical placement invariant.
        for (k, v) in &oracle {
            assert_eq!(*v, map.get(*k).unwrap());
        }
        expect_canonical(&map, &AutoContext::<u32>::default());
    }

    #[test]
    fn hash_map_get_or_put() {
        let (_buf, mut map): (_, M<u32, u32>) = new_map(32);

        for i in 0..10 {
            map.put(i * 2, 2).unwrap();
        }

        for i in 0..20 {
            let _ = map.get_or_put_value(i, 1).unwrap();
        }

        let mut sum = 0;
        for i in 0..20 {
            sum += map.get(i).unwrap();
        }

        assert_eq!(sum, 30);
    }

    #[test]
    fn hash_map_basic_hash_map_usage() {
        let (_buf, mut map): (_, M<i32, i32>) = new_map(32);

        assert!(map.fetch_put(1, 11).unwrap().is_none());
        assert!(map.fetch_put(2, 22).unwrap().is_none());
        assert!(map.fetch_put(3, 33).unwrap().is_none());
        assert!(map.fetch_put(4, 44).unwrap().is_none());

        map.put_no_clobber(5, 55).unwrap();
        assert!(map.fetch_put(5, 66).unwrap().unwrap().value == 55);
        assert!(map.fetch_put(5, 55).unwrap().unwrap().value == 66);

        unsafe {
            let gop1 = map.get_or_put(5).unwrap();
            assert!(gop1.found_existing);
            assert!(*gop1.value_ptr == 55);
            *gop1.value_ptr = 77;
            assert!(*map.get_entry(5).unwrap().value_ptr == 77);

            let gop2 = map.get_or_put(99).unwrap();
            assert!(!gop2.found_existing);
            *gop2.value_ptr = 42;
            assert!(*map.get_entry(99).unwrap().value_ptr == 42);

            let gop3 = map.get_or_put_value(5, 5).unwrap();
            assert!(*gop3.value_ptr == 77);

            let gop4 = map.get_or_put_value(100, 41).unwrap();
            assert!(*gop4.value_ptr == 41);

            assert!(map.contains(2));
            assert!(*map.get_entry(2).unwrap().value_ptr == 22);
            assert!(map.get(2).unwrap() == 22);
        }

        let rmv1 = map.fetch_remove(2);
        assert!(rmv1.unwrap().key == 2);
        assert!(rmv1.unwrap().value == 22);
        assert!(map.fetch_remove(2).is_none());
        assert!(!map.remove(2));
        assert!(map.get_entry(2).is_none());
        assert!(map.get(2).is_none());

        assert!(map.remove(3));
    }

    #[test]
    fn hash_map_ensure_unused_capacity() {
        let cap = 64;
        let (_buf, mut map): (_, M<u64, u64>) = new_map(cap);

        map.ensure_unused_capacity(32).unwrap();
        assert_eq!(Err(AllocError::OutOfMemory), map.ensure_unused_capacity(cap + 1));
    }

    #[test]
    fn hash_map_remove_by_ptr() {
        let (_buf, mut map): (_, M<i32, u64>) = new_map(64);

        for i in 0..10 {
            map.put(i, 0).unwrap();
        }

        assert!(map.count() == 10);

        for i in 0..10 {
            let key_ptr = map.get_key_ptr(i);
            assert!(key_ptr.is_some());

            if let Some(ptr) = key_ptr {
                map.remove_by_ptr(ptr);
            }
        }

        assert!(map.count() == 0);
    }

    #[test]
    fn hash_map_remove_by_ptr_0_sized_key() {
        let (_buf, mut map): (_, M<i32, u64>) = new_map(64);

        map.put(0, 0).unwrap();

        assert!(map.count() == 1);

        let key_ptr = map.get_key_ptr(0);
        assert!(key_ptr.is_some());

        if let Some(ptr) = key_ptr {
            map.remove_by_ptr(ptr);
        }

        assert!(map.count() == 0);
    }

    #[test]
    fn hash_map_repeat_fetch_remove() {
        let (_buf, mut map): (_, M<u64, ()>) = new_map(64);

        map.put_assume_capacity(0, ());
        map.put_assume_capacity(1, ());
        map.put_assume_capacity(2, ());
        map.put_assume_capacity(3, ());

        // fetchRemove() should make slots available.
        for _ in 0..10 {
            assert!(map.fetch_remove(3).is_some());
            map.put_assume_capacity(3, ());
        }

        assert!(map.get(0).is_some());
        assert!(map.get(1).is_some());
        assert!(map.get(2).is_some());
        assert!(map.get(3).is_some());
    }

    #[test]
    fn offset_hash_map_basic_usage() {
        type OffsetMap = AutoOffsetHashMap<u32, u32, DEFAULT_MAX_LOAD_PERCENTAGE>;
        let cap = 16;

        let layout = OffsetMap::layout(cap);
        let buf = AlignedBuf::new(layout.total_size, OffsetMap::BASE_ALIGN);
        let offset_map = unsafe { OffsetMap::init(OffsetBuf::init(buf.ptr()), layout) };
        let mut map = unsafe { offset_map.map(buf.ptr()) };

        let count = 5;
        let mut total: u32 = 0;
        for i in 0..count {
            map.put(i, i).unwrap();
            total += i;
        }

        let mut sum: u32 = 0;
        for kv in map.iterator() {
            sum += unsafe { *kv.key_ptr };
        }
        assert_eq!(total, sum);

        sum = 0;
        for i in 0..count {
            assert_eq!(i, map.get(i).unwrap());
            sum += map.get(i).unwrap();
        }
        assert_eq!(total, sum);
    }

    #[test]
    fn offset_hash_map_remake_map() {
        type OffsetMap = AutoOffsetHashMap<u32, u32, DEFAULT_MAX_LOAD_PERCENTAGE>;
        let cap = 16;

        let layout = OffsetMap::layout(cap);
        let buf = AlignedBuf::new(layout.total_size, OffsetMap::BASE_ALIGN);
        let offset_map = unsafe { OffsetMap::init(OffsetBuf::init(buf.ptr()), layout) };

        {
            let mut map = unsafe { offset_map.map(buf.ptr()) };
            map.put(5, 5).unwrap();
        }

        {
            let map = unsafe { offset_map.map(buf.ptr()) };
            assert_eq!(5, map.get(5).unwrap());
        }
    }

    #[test]
    fn offset_hash_map_maximum_load_leaves_probe_headroom() {
        type OffsetMap = AutoOffsetHashMap<u32, u32, 80>;
        let requested_size: u32 = 16;
        let layout = OffsetMap::layout(requested_size);
        let buf = AlignedBuf::new(layout.total_size, OffsetMap::BASE_ALIGN);

        let offset_map = unsafe { OffsetMap::init(OffsetBuf::init(buf.ptr()), layout) };
        let mut map = unsafe { offset_map.map(buf.ptr()) };

        assert!(map.capacity() > requested_size);
        assert!(map.max_load() >= requested_size);
        assert!(map.max_load() < map.capacity());

        for i in 0..requested_size {
            map.put(i, i).unwrap();
        }
        for _ in 0..100 {
            for i in 0..requested_size {
                assert!(map.remove(i));
                map.put(i, i).unwrap();
            }
        }

        for i in 0..requested_size {
            assert_eq!(Some(i), map.get(i));
        }
    }

    #[test]
    fn layout_for_capacity_no_overflow_for_large_capacity() {
        // Test that layoutForCapacity correctly handles large capacities without overflow.
        // Prior to the fix, new_capacity (u32) was multiplied before widening to usize,
        // causing overflow when new_capacity * @sizeOf(K) exceeded 2^32.
        // See: https://github.com/ghostty-org/ghostty/issues/9862
        type Map = M<u64, u64>;

        // Use 2^30 capacity - this would overflow in u32 when multiplied by @sizeOf(u64)=8
        // 0x40000000 * 8 = 0x2_0000_0000 which wraps to 0 in u32
        let large_cap: Size = 1 << 30;
        let layout = Map::layout_for_capacity(large_cap);

        // With the fix, total_size should be at least cap * (sizeof(K) + sizeof(V))
        // = 2^30 * 16 = 2^34 bytes = 16 GiB
        // Without the fix, this would wrap and produce a much smaller value.
        let min_expected: usize = large_cap as usize * (core::mem::size_of::<u64>() + core::mem::size_of::<u64>());
        assert!(layout.total_size >= min_expected);

        // Also verify the individual offsets don't wrap
        assert!(layout.keys_start > 0);
        assert!(layout.vals_start > layout.keys_start);
    }
}
