//! Port of `src/datastruct/comptime_int_set.zig`.
//!
//! STATUS: ported
//!
//! The Zig type is parameterized by a comptime slice of keys. Rust can't
//! use slices of a generic type as const generic parameters, so the port
//! is a value built by a `const fn` (sorting happens during constant
//! evaluation when stored in a `const`/`static`), parameterized by the
//! integer type and the number of keys:
//!
//! ```ignore
//! const IDS: ComptimeIntSet<u16, 3> = ComptimeIntSet::<u16, 3>::new(&[2026, 25, 1049]);
//! IDS.index_of(25) // Some(1)
//! ```
//!
//! Duplicate keys panic inside the `const fn`, which is a compile error
//! when the set is built in a `const` (the Zig `@compileError`).

/// A fixed set of integers that is known at compile time and can be
/// searched quickly at runtime.
///
/// Use this when you have a table of integer IDs defined at comptime and
/// need to find which entry a runtime value belongs to. The common way to
/// write that is a loop over the table:
///
/// ```text
///     for (i, entry) in entries.iter().enumerate() {
///         if entry.id == id { return Some(i); }
///     }
///     None
/// ```
///
/// That loop compiles to one comparison per entry. Entries near the end
/// of the table, and values that aren't in the table at all, pay for every
/// comparison before them. This type sorts the keys at comptime and does
/// a binary search instead, so every lookup costs about log2(N)
/// comparisons no matter which key is looked up. The search has a fixed
/// number of steps (N is a const generic, so the loop is unrolled), which
/// lets the compiler emit it without branches.
///
/// `index_of` returns the position of the key in the `keys` slice you
/// passed in, not its position in the sorted order. This means you can
/// use the result to index any other table that is in the same order as
/// `keys`.
///
/// `T` must be an integer type. Keys can be in any order but must be
/// unique. Duplicate keys are a compile error (when built in a const).
///
/// For a handful of keys a plain loop or `match` is just as fast and
/// simpler. This is worth reaching for when the table has dozens of
/// entries and the lookup is on a hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComptimeIntSet<T, const N: usize> {
    /// The keys in ascending order.
    sorted: [T; N],

    /// For each sorted key, its index in the original `keys`
    /// slice. That is, `sorted[i] == keys[original[i]]`.
    ///
    /// Zig uses the smallest unsigned integer that fits every index
    /// (`Index`); this port uses `usize`.
    original: [usize; N],
}

/// The type returned by `index_of`.
pub type Index = usize;

macro_rules! impl_comptime_int_set {
    ($($t:ty),* $(,)?) => {$(
        impl<const N: usize> ComptimeIntSet<$t, N> {
            /// The number of keys in the set.
            pub const LEN: usize = N;

            /// Build the set from the given keys. Panics (a compile error
            /// in const context) if the keys are not unique.
            pub const fn new(keys: &[$t; N]) -> Self {
                let mut order = [0usize; N];
                let mut i = 0;
                while i < N {
                    order[i] = i;
                    i += 1;
                }

                // Insertion sort on the indices by key.
                let mut i = 1;
                while i < N {
                    let mut j = i;
                    while j > 0 && keys[order[j - 1]] > keys[order[j]] {
                        let tmp = order[j - 1];
                        order[j - 1] = order[j];
                        order[j] = tmp;
                        j -= 1;
                    }
                    i += 1;
                }

                let mut sorted = [0 as $t; N];
                let mut i = 0;
                while i < N {
                    sorted[i] = keys[order[i]];
                    if i > 0 && sorted[i - 1] == sorted[i] {
                        panic!("ComptimeIntSet keys must be unique");
                    }
                    i += 1;
                }

                Self { sorted, original: order }
            }

            /// Returns the index of `key` in the original `keys` slice, or
            /// None if `key` is not in the set.
            #[inline(always)]
            pub fn index_of(&self, key: $t) -> Option<Index> {
                if N == 0 {
                    return None;
                }

                // Binary search for the last sorted key that is <= `key`.
                // Each step halves the range that is left. `n` is comptime
                // known, so this unrolls into a fixed number of steps.
                let mut base: usize = 0;
                let mut n: usize = N;
                while n > 1 {
                    let half = n / 2;
                    base = if self.sorted[base + half] <= key { base + half } else { base };
                    n -= half;
                }

                if self.sorted[base] != key {
                    return None;
                }
                Some(self.original[base])
            }

            /// Returns true if `key` is in the set.
            #[inline(always)]
            pub fn contains(&self, key: $t) -> bool {
                self.index_of(key).is_some()
            }
        }
    )*};
}

impl_comptime_int_set!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comptime_int_set_empty() {
        const SET: ComptimeIntSet<u16, 0> = ComptimeIntSet::<u16, 0>::new(&[]);
        assert!(SET.index_of(0).is_none());
        assert!(!SET.contains(1));
    }

    #[test]
    fn comptime_int_set_single() {
        const SET: ComptimeIntSet<u16, 1> = ComptimeIntSet::<u16, 1>::new(&[42]);
        assert_eq!(0, SET.index_of(42).unwrap());
        assert!(SET.index_of(41).is_none());
        assert!(SET.index_of(43).is_none());
    }

    #[test]
    fn comptime_int_set_unsorted_keys_return_original_index() {
        const KEYS: [u16; 7] = [2026, 4, 1049, 25, 1, 65535, 0];
        const SET: ComptimeIntSet<u16, 7> = ComptimeIntSet::<u16, 7>::new(&KEYS);
        for (i, &key) in KEYS.iter().enumerate() {
            assert_eq!(i, SET.index_of(key).unwrap());
        }
    }

    fn exhaustive<const N: usize>() {
        let keys: [u8; N] = std::array::from_fn(|i| ((i * 37 + 11) % 251) as u8);
        let set = ComptimeIntSet::<u8, N>::new(&keys);

        for v in 0..256usize {
            let expected: Option<usize> = keys.iter().position(|&key| key as usize == v);
            let actual: Option<usize> = set.index_of(v as u8);
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn comptime_int_set_exhaustive_against_linear_scan() {
        // The search unrolls differently for each length, so test every
        // length from 0 up past a few powers of two.
        macro_rules! each {
            ($($n:literal)*) => { $(exhaustive::<$n>();)* };
        }
        each!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19);
    }

    #[test]
    fn comptime_int_set_signed() {
        const SET: ComptimeIntSet<i8, 5> = ComptimeIntSet::<i8, 5>::new(&[5, -3, 0, -128, 127]);
        assert_eq!(1, SET.index_of(-3).unwrap());
        assert_eq!(3, SET.index_of(-128).unwrap());
        assert_eq!(4, SET.index_of(127).unwrap());
        assert!(SET.index_of(-4).is_none());
    }
}
