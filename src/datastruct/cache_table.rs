//! Port of `src/datastruct/cache_table.zig`.
//!
//! STATUS: ported

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::MaybeUninit;

use crate::fastmem;
use crate::quirks::inline_assert as assert;

/// Behaviors of a [`CacheTable`] (the Zig `Context` type).
///
///   - `fn hash(&self, K) -> u64`    - Return a hash for a key.
///   - `fn eql(&self, K, K) -> bool` - Check two keys for equality.
///
///   - `fn evicted(&mut self, K, V)` - [OPTIONAL] Eviction callback.
///     If overridden, called whenever an item is evicted from the cache.
pub trait CacheTableContext<K, V> {
    fn hash(&self, key: &K) -> u64;
    fn eql(&self, a: &K, b: &K) -> bool;

    /// Eviction hook. The default does nothing (Zig: `evicted` absent).
    /// The evicted value is passed by reference because `put` also
    /// returns it to the caller.
    #[inline]
    fn evicted(&mut self, key: &K, value: &V) {
        let _ = (key, value);
    }
}

/// A key/value pair stored in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KV<K, V> {
    pub key: K,
    pub value: V,
}

/// An associative data structure used for efficiently storing and
/// retrieving values which are able to be recomputed if necessary.
///
/// This structure is effectively a hash table with fixed-sized buckets.
///
/// When inserting an item in to a full bucket, the least recently used
/// item is replaced.
///
/// To achieve this, when an item is accessed, it's moved to the end of
/// the bucket, and the rest of the items are moved over to fill the gap.
///
/// This should provide very good query performance and keep frequently
/// accessed items cached indefinitely.
///
/// Parameters:
///
/// `C` (Context)
///   A type implementing [`CacheTableContext`].
///
/// `BUCKET_COUNT`
///   Should ideally be close to the median number of important items that
///   you expect to be cached at any given point. This is required to be a
///   power of 2 since performance suffers if it's not and there's no good
///   reason to allow it to be anything else.
///
/// `BUCKET_SIZE`
///   should be larger if you expect a large number of unimportant items to
///   enter the cache at a time. Having larger buckets will avoid important
///   items being dropped from the cache prematurely. Must fit in a `u8`.
pub struct CacheTable<K, V, C, const BUCKET_COUNT: usize, const BUCKET_SIZE: usize>
where
    C: CacheTableContext<K, V>,
{
    /// `bucket_count` buckets containing `bucket_size` KV pairs each.
    ///
    /// We don't need to initialize this memory because we don't use it
    /// unless it's within a bucket's stored length, which will guarantee
    /// that we put actual items there.
    pub buckets: [[MaybeUninit<KV<K, V>>; BUCKET_SIZE]; BUCKET_COUNT],

    /// We use this array to keep track of how many slots in each bucket
    /// have actual items in them. Once all the buckets fill up this will
    /// become a pointless check, but hopefully branch prediction picks
    /// up on it at that point. The memory cost isn't too bad since it's
    /// just bytes, so should be a fraction the size of the main table.
    pub lengths: [u8; BUCKET_COUNT],

    /// An instance of the context structure.
    pub context: C,
}

impl<K, V, C, const BUCKET_COUNT: usize, const BUCKET_SIZE: usize>
    CacheTable<K, V, C, BUCKET_COUNT, BUCKET_SIZE>
where
    C: CacheTableContext<K, V>,
{
    const CHECK: () = {
        assert!(BUCKET_COUNT.is_power_of_two());
        assert!(BUCKET_SIZE > 0 && BUCKET_SIZE <= u8::MAX as usize);
    };

    /// Create an empty table with the given context.
    pub fn new(context: C) -> Self {
        #[allow(clippy::let_unit_value)]
        let () = Self::CHECK;
        Self {
            buckets: [const { [const { MaybeUninit::uninit() }; BUCKET_SIZE] }; BUCKET_COUNT],
            lengths: [0; BUCKET_COUNT],
            context,
        }
    }

    /// The initialized items of bucket `idx`.
    pub fn bucket(&self, idx: usize) -> &[KV<K, V>] {
        let len = self.lengths[idx] as usize;
        // SAFETY: the first `lengths[idx]` slots of a bucket are initialized.
        unsafe {
            &*(&self.buckets[idx][..len] as *const [MaybeUninit<KV<K, V>>] as *const [KV<K, V>])
        }
    }

    fn bucket_mut(&mut self, idx: usize, len: usize) -> &mut [KV<K, V>] {
        debug_assert!(len <= self.lengths[idx] as usize);
        // SAFETY: the first `lengths[idx]` slots of a bucket are initialized.
        unsafe {
            &mut *(&mut self.buckets[idx][..len] as *mut [MaybeUninit<KV<K, V>>]
                as *mut [KV<K, V>])
        }
    }

    /// Adds an item to the cache table. If an old value was removed to
    /// make room then it is returned in a struct with its key and value.
    pub fn put(&mut self, key: K, value: V) -> Option<KV<K, V>> {
        let kv = KV { key, value };
        let idx = (self.context.hash(&kv.key) % BUCKET_COUNT as u64) as usize;

        // If we have space available in the bucket then we just append
        if (self.lengths[idx] as usize) < BUCKET_SIZE {
            self.buckets[idx][self.lengths[idx] as usize].write(kv);
            self.lengths[idx] += 1;
            return None;
        }
        assert(self.lengths[idx] as usize == BUCKET_SIZE);

        // Append our new item and return the oldest
        let evicted = fastmem::rotate_in(self.bucket_mut(idx, BUCKET_SIZE), kv);

        // The Context is allowed to register an eviction hook.
        self.context.evicted(&evicted.key, &evicted.value);

        Some(evicted)
    }

    /// Retrieves an item from the cache table.
    ///
    /// Returns None if no item is found with the provided key.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let idx = (self.context.hash(key) % BUCKET_COUNT as u64) as usize;
        let len = self.lengths[idx] as usize;
        let mut i: usize = len;
        while i > 0 {
            i -= 1;
            if self.context.eql(key, &self.bucket(idx)[i].key) {
                // Move the accessed item to the end of the bucket.
                let bucket = self.bucket_mut(idx, len);
                fastmem::rotate_once(&mut bucket[i..len]);
                return Some(&self.bucket(idx)[len - 1].value);
            }
        }

        None
    }

    /// Removes all items from the cache table.
    ///
    /// If your `Context` has an `evicted` method,
    /// it will be called with all removed items.
    pub fn clear(&mut self) {
        for idx in 0..BUCKET_COUNT {
            let len = self.lengths[idx] as usize;
            // Reset first so a panicking hook can only leak, never
            // double-drop.
            self.lengths[idx] = 0;
            for i in 0..len {
                // SAFETY: slot i < len was initialized and is moved out
                // exactly once (the length is already reset).
                let kv = unsafe { self.buckets[idx][i].assume_init_read() };
                self.context.evicted(&kv.key, &kv.value);
                drop(kv);
            }
        }
    }
}

impl<K, V, C, const BUCKET_COUNT: usize, const BUCKET_SIZE: usize> Drop
    for CacheTable<K, V, C, BUCKET_COUNT, BUCKET_SIZE>
where
    C: CacheTableContext<K, V>,
{
    fn drop(&mut self) {
        // Drop the stored items without invoking the eviction hook.
        for idx in 0..BUCKET_COUNT {
            let len = self.lengths[idx] as usize;
            for i in 0..len {
                // SAFETY: slot i < len is initialized.
                unsafe { self.buckets[idx][i].assume_init_drop() };
            }
            self.lengths[idx] = 0;
        }
    }
}

/// Creates a Context automatically for the given key type. This uses the
/// same logic as std.hash_map.AutoContext today since the API matches.
pub struct AutoContext<K>(PhantomData<fn(&K)>);

impl<K> Default for AutoContext<K> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<K: Hash + Eq, V> CacheTableContext<K, V> for AutoContext<K> {
    fn hash(&self, key: &K) -> u64 {
        let mut h = DefaultHasher::new();
        key.hash(&mut h);
        h.finish()
    }

    fn eql(&self, a: &K, b: &K) -> bool {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_table() {
        // Construct a table that purposely has a predictable hash so we can
        // test all edge cases.
        struct Ctx;
        impl CacheTableContext<u32, u32> for Ctx {
            fn hash(&self, key: &u32) -> u64 {
                *key as u64
            }

            fn eql(&self, a: &u32, b: &u32) -> bool {
                a == b
            }
        }
        type T = CacheTable<u32, u32, Ctx, 2, 2>;
        let mut t: T = T::new(Ctx);

        // Fill the table
        assert!(t.put(0, 0).is_none());
        assert!(t.put(1, 0).is_none());
        assert!(t.put(2, 0).is_none());
        assert!(t.put(3, 0).is_none());

        // It should now be full, so any insert should evict the oldest item.
        // NOTE: For the sake of this test, we're assuming that the first item
        // is evicted but we don't need to promise this.
        assert_eq!(KV { key: 0, value: 0 }, t.put(4, 0).unwrap());

        // The first item should now be gone
        assert!(t.get(&0).is_none());
    }

    #[test]
    fn eviction_hook_and_lru() {
        // Not in the Zig file: exercises the optional eviction hook, the
        // LRU move-to-end on get, and dropping of owned values.
        #[derive(Default)]
        struct Ctx {
            evicted: Vec<(u32, String)>,
        }
        impl CacheTableContext<u32, String> for Ctx {
            fn hash(&self, _key: &u32) -> u64 {
                0
            }

            fn eql(&self, a: &u32, b: &u32) -> bool {
                a == b
            }

            fn evicted(&mut self, key: &u32, value: &String) {
                self.evicted.push((*key, value.clone()));
            }
        }
        let mut t: CacheTable<u32, String, Ctx, 1, 2> = CacheTable::new(Ctx::default());
        assert!(t.put(1, "a".into()).is_none());
        assert!(t.put(2, "b".into()).is_none());
        // Touch 1 so 2 becomes the oldest.
        assert_eq!(t.get(&1).map(String::as_str), Some("a"));
        let ev = t.put(3, "c".into()).unwrap();
        assert_eq!((ev.key, ev.value.as_str()), (2, "b"));
        assert_eq!(t.context.evicted, vec![(2, "b".to_string())]);
        t.clear();
        assert_eq!(t.context.evicted.len(), 3);
        assert!(t.get(&1).is_none());
    }
}
