//! Port of `src/synthetic/main.zig`.
//!
//! STATUS: ported
//!
//! The synthetic package contains an abstraction for generating
//! synthetic data. The motivating use case for this package is to
//! generate synthetic data for benchmarking, but it may also expand
//! to other use cases such as fuzzing (e.g. to generate a corpus
//! rather than directly fuzzing).
//!
//! The generators in this package are typically not performant
//! enough to be streamed in real time. They should instead be
//! used to generate a large amount of data in a single go
//! and then streamed from there.
//!
//! The generators are aimed for terminal emulation, but the package
//! is not limited to that and we may want to extract this to a
//! standalone package one day.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "bytes.rs"]
pub mod bytes;
#[path = "cli.rs"]
pub mod cli;
#[path = "generator.rs"]
pub mod generator;
#[path = "kitty.rs"]
pub mod kitty;
#[path = "osc.rs"]
pub mod osc;
#[path = "utf8.rs"]
pub mod utf8;
// @generated-mods-end

use std::cell::Cell;
use std::rc::Rc;

pub use self::bytes::Bytes;
pub use self::generator::{FixedWriter, Generator};
pub use self::kitty::Kitty;
pub use self::osc::Osc;
pub use self::utf8::Utf8;

// ---------------------------------------------------------------------------
// Zig `std.Random` / `std.Random.DefaultPrng`
// ---------------------------------------------------------------------------
//
// The synthetic generators take a `std.Random` interface by value. A Zig
// `std.Random` is a (pointer, fill function) pair: every copy refers to the
// same underlying PRNG state. The Rust counterpart is [`Random`], a cheap
// clonable handle sharing the PRNG state through an `Rc`.
//
// The algorithms (xoshiro256++ seeded via splitmix64, `int`, `boolean`,
// `uintLessThan` (Lemire), `uintLessThanBiased`, `intRangeAtMost(Biased)`,
// `float(f64)` and `weightedIndex`) mirror Zig's standard library so the
// generated sequences match Zig's for the same seed.
//
// Zig's integer helpers are generic over the integer type; its bit width
// determines how many random bytes are consumed. In Rust the bit width is
// a const generic parameter `B` (1..=64) and values are passed as `u64`.

/// Zig `std.Random.SplitMix64`.
#[derive(Debug, Clone, Copy)]
pub struct SplitMix64 {
    s: u64,
}

impl SplitMix64 {
    pub fn init(seed: u64) -> SplitMix64 {
        SplitMix64 { s: seed }
    }

    pub fn next(&mut self) -> u64 {
        self.s = self.s.wrapping_add(0x9e3779b97f4a7c15);

        let mut z = self.s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
}

/// Zig `std.Random.Xoshiro256` (xoshiro256++), which is
/// `std.Random.DefaultPrng`.
#[derive(Debug, Clone)]
pub struct DefaultPrng {
    s: Rc<Cell<[u64; 4]>>,
}

impl DefaultPrng {
    pub fn init(init_s: u64) -> DefaultPrng {
        let mut gen = SplitMix64::init(init_s);
        let s = [gen.next(), gen.next(), gen.next(), gen.next()];
        DefaultPrng {
            s: Rc::new(Cell::new(s)),
        }
    }

    /// Return a [`Random`] interface that shares this PRNG's state.
    pub fn random(&self) -> Random {
        Random { s: self.s.clone() }
    }
}

/// Zig `std.Random`: a handle to a shared PRNG. Clones share state, like
/// copies of a Zig `std.Random` interface.
#[derive(Debug, Clone)]
pub struct Random {
    s: Rc<Cell<[u64; 4]>>,
}

impl Random {
    fn next_u64(&self) -> u64 {
        let mut s = self.s.get();
        let r = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);

        let t = s[1] << 17;

        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];

        s[2] ^= t;

        s[3] = s[3].rotate_left(45);
        self.s.set(s);

        r
    }

    /// Read "random" bytes into a buffer (Xoshiro256 `fill`).
    pub fn bytes(&self, buf: &mut [u8]) {
        let mut i: usize = 0;
        let aligned_len = buf.len() - (buf.len() & 7);

        // Complete 8 byte segments.
        while i < aligned_len {
            let mut n = self.next_u64();
            for j in 0..8 {
                buf[i + j] = n as u8;
                n >>= 8;
            }
            i += 8;
        }

        // Remaining. (cuts off and loses some bytes)
        if i != buf.len() {
            let mut n = self.next_u64();
            while i < buf.len() {
                buf[i] = n as u8;
                n >>= 8;
                i += 1;
            }
        }
    }

    /// Returns a random int `i` such that `minInt(T) <= i <= maxInt(T)`
    /// for an unsigned `T` of `B` bits.
    pub fn int<const B: u32>(&self) -> u64 {
        const { assert!(B >= 1 && B <= 64) };
        let ceil_bytes = B.div_ceil(8) as usize;
        let mut rand_bytes = [0u8; 8];
        self.bytes(&mut rand_bytes[..ceil_bytes]);
        let byte_aligned_result = u64::from_le_bytes(rand_bytes);
        truncate::<B>(byte_aligned_result)
    }

    /// Returns a random bool.
    pub fn boolean(&self) -> bool {
        self.int::<1>() != 0
    }

    /// Constant-time implementation of `uintLessThan`. The results of
    /// this function may be biased.
    pub fn uint_less_than_biased<const B: u32>(&self, less_than: u64) -> u64 {
        debug_assert!(0 < less_than);
        limit_range_biased::<B>(self.int::<B>(), less_than)
    }

    /// Returns an evenly distributed random unsigned integer
    /// `0 <= i < less_than`. This function assumes that the underlying
    /// PRNG provides a uniform distribution (Lemire's method).
    pub fn uint_less_than<const B: u32>(&self, less_than: u64) -> u64 {
        debug_assert!(0 < less_than);

        let mut x = self.int::<B>();
        let mut m: u128 = x as u128 * less_than as u128;
        let mut l = truncate::<B>(m as u64);
        if l < less_than {
            let mut t = truncate::<B>(less_than.wrapping_neg());
            if t >= less_than {
                t -= less_than;
                if t >= less_than {
                    t %= less_than;
                }
            }
            while l < t {
                x = self.int::<B>();
                m = x as u128 * less_than as u128;
                l = truncate::<B>(m as u64);
            }
        }
        (m >> B) as u64
    }

    /// Returns an evenly distributed random unsigned integer
    /// `0 <= i <= at_most`.
    pub fn uint_at_most<const B: u32>(&self, at_most: u64) -> u64 {
        if at_most == max_int::<B>() {
            // have the full range
            return self.int::<B>();
        }
        self.uint_less_than::<B>(at_most + 1)
    }

    /// Constant-time implementation of `uintAtMost`. The results of this
    /// function may be biased.
    pub fn uint_at_most_biased<const B: u32>(&self, at_most: u64) -> u64 {
        if at_most == max_int::<B>() {
            // have the full range
            return self.int::<B>();
        }
        self.uint_less_than_biased::<B>(at_most + 1)
    }

    /// Returns an evenly distributed random integer
    /// `at_least <= i <= at_most` (unsigned `T` of `B` bits).
    pub fn int_range_at_most<const B: u32>(&self, at_least: u64, at_most: u64) -> u64 {
        debug_assert!(at_least <= at_most);
        at_least + self.uint_at_most::<B>(at_most - at_least)
    }

    /// Constant-time implementation of `intRangeAtMost`. The results of
    /// this function may be biased.
    pub fn int_range_at_most_biased<const B: u32>(&self, at_least: u64, at_most: u64) -> u64 {
        debug_assert!(at_least <= at_most);
        at_least + self.uint_at_most_biased::<B>(at_most - at_least)
    }

    /// Return a floating point value evenly distributed in the range
    /// [0, 1).
    pub fn float(&self) -> f64 {
        // Use 64-bit integer to cover the range.
        let rand = self.int::<64>();
        let mut rand_lz: u64 = rand.leading_zeros() as u64;
        if rand_lz >= 41 {
            rand_lz = 41 + self.int::<64>().leading_zeros() as u64;
            if rand_lz == 41 + 64 {
                // It is astronomically unlikely to reach this point.
                rand_lz += ((self.int::<32>() as u32) | 0x7FF).leading_zeros() as u64;
            }
        }
        let mantissa: u64 = rand & 0xFFFFFFFFFFFFF;
        let exponent: u64 = (1022 - rand_lz) << 52;
        f64::from_bits(exponent | mantissa)
    }

    /// Randomly selects an index into `proportions`, where the likelihood
    /// of each index is weighted by that proportion.
    ///
    /// This is useful for selecting an item from a slice where weights
    /// are not equal. `proportions` must be non-empty with a positive sum.
    pub fn weighted_index(&self, proportions: &[f64]) -> usize {
        let sum: f64 = proportions.iter().fold(0.0, |acc, v| acc + v);
        let point = self.float() * sum;
        debug_assert!(point < sum);

        let mut accumulator: f64 = 0.0;
        for (index, p) in proportions.iter().enumerate() {
            accumulator += p;
            if point < accumulator {
                return index;
            }
        }
        unreachable!()
    }
}

#[inline]
fn max_int<const B: u32>() -> u64 {
    if B >= 64 {
        u64::MAX
    } else {
        (1u64 << B) - 1
    }
}

#[inline]
fn truncate<const B: u32>(v: u64) -> u64 {
    v & max_int::<B>()
}

/// Convert a random integer 0 <= random_int <= maxValue(T),
/// into an integer 0 <= result < less_than.
/// This function introduces a minor bias.
fn limit_range_biased<const B: u32>(random_int: u64, less_than: u64) -> u64 {
    // adapted from:
    //   http://www.pcg-random.org/posts/bounded-rands.html
    //   "Integer Multiplication (Biased)"
    let m: u128 = random_int as u128 * less_than as u128;
    (m >> B) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not part of synthetic/main.zig (whose only test is `refAllDecls`):
    // checks the PRNG matches Zig's xoshiro256++ "sequence" test.
    #[test]
    fn xoshiro256_sequence() {
        let r = Random {
            s: Rc::new(Cell::new([0x1, 0x2, 0x3, 0x4])),
        };
        let seq = [
            41943041u64,
            58720359,
            3588806011781223,
            3591011842654386,
            9228616714210784205,
            9973669472204895162,
        ];
        for s in seq {
            assert_eq!(s, r.next_u64());
        }
    }

    #[test]
    fn random_ranges() {
        let prng = DefaultPrng::init(0);
        let r = prng.random();
        for _ in 0..1000 {
            let v = r.int_range_at_most::<64>(2, 5);
            assert!((2..=5).contains(&v));
            let v = r.int_range_at_most_biased::<21>(0x80, 0x7FF);
            assert!((0x80..=0x7FF).contains(&v));
            assert!(r.uint_less_than::<8>(8) < 8);
            let f = r.float();
            assert!((0.0..1.0).contains(&f));
            assert_eq!(1, r.weighted_index(&[0.0, 1.0, 0.0]));
        }
    }
}
