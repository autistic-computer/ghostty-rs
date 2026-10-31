//! Port of `src/terminal/compress/lz4_differential.zig`.
//!
//! STATUS: ported
//!
//! Differential and property tests for the LZ4 block codec.
//!
//! Every generated input must compress into a block which:
//!
//!   1. fits within `compressBound`,
//!   2. is structurally valid LZ4 with the stricter guarantees our
//!      compressor documents (final five bytes are literals, matches start
//!      at least twelve bytes before the end), verified by an independent
//!      walker which shares no code with the codec,
//!   3. decompresses to exactly the original bytes, and
//!   4. is rejected when decompressed into a buffer of the wrong size.
//!
//! Valid blocks are additionally mutated (bit flips, splices, truncations)
//! and fed to the decompressor, which must fail cleanly or succeed, but
//! never read or write out of bounds. Rust slice indexing is always bounds
//! checked, so any out-of-bounds access fails the test.
//!
//! The light suite below runs as a normal unit test and finishes quickly.
//! The exhaustive suite multiplies the same properties across far more
//! sizes, periods, seeds, and mutations; it is slow and therefore skipped
//! unless the environment variable `GHOSTTY_LZ4_SLOW` is set:
//!
//!     GHOSTTY_LZ4_SLOW=1 cargo test --lib lz4_differential
//!
//! The random stream comes from a deterministic xoshiro256++ stand-in for
//! Zig's `std.Random.DefaultPrng`; the exact sequence differs from Zig's.

#![cfg(test)]

use super::lz4;
use super::lz4::test_rng::Prng;

/// Number of trailing block bytes which must be literals, mirroring the
/// documented guarantee of the compressor. Kept as an independent constant
/// so a codec regression cannot silently weaken the check.
const LAST_LITERALS: usize = 5;

/// A match may not begin in the final twelve bytes. See `last_literals`.
const MATCH_FIND_LIMIT: usize = 12;

/// Sizes around every encoding boundary: token nibble limits (14/15),
/// minimum match and find limits (4/12), length-extension steps (15 + 255k),
/// and power-of-two neighborhoods.
const BOUNDARY_SIZES: [usize; 38] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 31, 32, 33, 63, 64, 65, 254, 255, 256,
    269, 270, 271, 272, 1023, 1024, 4095, 4096, 4097,
];

/// Input generators exercising distinct codec behaviors. Every generator is
/// deterministic for a given random state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Generator {
    /// Uniform random bytes; largely incompressible.
    RandomBytes,

    /// Runs of one repeated byte with random lengths; period-one matches.
    Runs,

    /// One repeating pattern; exercises a fixed match period end to end.
    Periodic,

    /// Eight-byte records with random small payloads and zero padding, with
    /// some records repeated; resembles terminal cell memory.
    Cells,

    /// Dictionary words with separators; text-like literal/match mix.
    Words,

    /// Mostly zeros with scattered random bytes; long matches with isolated
    /// literals.
    Sparse,

    /// Random segments of all other generators; exercises transitions.
    Mixed,
}

impl Generator {
    const ALL: [Generator; 7] = [
        Generator::RandomBytes,
        Generator::Runs,
        Generator::Periodic,
        Generator::Cells,
        Generator::Words,
        Generator::Sparse,
        Generator::Mixed,
    ];

    /// Zig: `random.enumValue(Generator)`.
    fn random(random: &mut Prng) -> Generator {
        Self::ALL[random.uint_less_than(Self::ALL.len())]
    }

    fn fill(self, random: &mut Prng, buf: &mut [u8]) {
        match self {
            Generator::RandomBytes => random.bytes(buf),

            Generator::Runs => {
                let mut i: usize = 0;
                while i < buf.len() {
                    let run = random.int_range_at_most(1, 300).min(buf.len() - i);
                    let v = random.int_u8();
                    buf[i..i + run].fill(v);
                    i += run;
                }
            }

            Generator::Periodic => {
                let period = random.int_range_at_most(1, 40);
                fill_periodic(random, buf, period);
            }

            Generator::Cells => {
                let mut i: usize = 0;
                while i + 8 <= buf.len() {
                    if i >= 8 && random.boolean() {
                        // Repeat one of the last 256 records.
                        let back = 8 * random.int_range_at_most(1, (i / 8).min(256));
                        buf.copy_within(i - back..i - back + 8, i);
                    } else {
                        let cell = &mut buf[i..i + 8];
                        cell.fill(0);
                        cell[0] = b' ' + random.uint_less_than(95) as u8;
                        cell[1] = random.uint_less_than(4) as u8;
                    }
                    i += 8;
                }
                random.bytes(&mut buf[i..]);
            }

            Generator::Words => {
                const WORDS: [&[u8]; 24] = [
                    b"the", b"terminal", b"page", b"compress", b"row", b"cell", b"style", b"zig", b"lz4",
                    b"block", b"offset", b"match", b"a", b"of", b"and", b"literal", b"0x00", b"0xFF", b"    ",
                    b"\r\n", b"-----", b"=", b"pub fn", b"const",
                ];
                let mut i: usize = 0;
                while i < buf.len() {
                    let word = WORDS[random.uint_less_than(WORDS.len())];
                    let n = word.len().min(buf.len() - i);
                    buf[i..i + n].copy_from_slice(&word[..n]);
                    i += n;
                    if i < buf.len() {
                        buf[i] = if random.boolean() { b' ' } else { b'\n' };
                        i += 1;
                    }
                }
            }

            Generator::Sparse => {
                buf.fill(0);
                if buf.is_empty() {
                    return;
                }
                for _ in 0..buf.len() / 32 + 1 {
                    let at = random.uint_less_than(buf.len());
                    buf[at] = random.int_u8();
                }
            }

            Generator::Mixed => {
                let mut i: usize = 0;
                while i < buf.len() {
                    let segment = random.int_range_at_most(1, 2048).min(buf.len() - i);
                    let sub = Generator::random(random);
                    if sub != Generator::Mixed {
                        sub.fill(random, &mut buf[i..i + segment]);
                    }
                    i += segment;
                }
            }
        }
    }
}

/// Fill `buf` with one repeating pattern of the given period.
fn fill_periodic(random: &mut Prng, buf: &mut [u8], period: usize) {
    if buf.is_empty() {
        return;
    }
    let head = period.min(buf.len());
    random.bytes(&mut buf[..head]);
    for i in head..buf.len() {
        buf[i] = buf[i - period];
    }
}

/// Reusable buffers sized for the largest input a suite generates.
struct Workspace {
    input: Vec<u8>,
    encoded: Vec<u8>,
    decoded: Vec<u8>,
    table: Box<lz4::HashTable>,
}

impl Workspace {
    fn init(max_input: usize) -> Workspace {
        Workspace {
            input: vec![0; max_input],
            encoded: vec![0; lz4::compress_bound(max_input).unwrap()],
            // One extra byte so wrong-size decompression can be tested above
            // the exact length as well as below it.
            decoded: vec![0; max_input + 1],
            table: Box::new([0; 4096]),
        }
    }
}

/// Compress one input and verify every property promised by the codec.
/// Returns the encoded length so callers can reuse the encoded block.
fn expect_codec_properties(ws: &mut Workspace, len: usize) -> usize {
    let input = &ws.input[..len];
    let encoded_len = lz4::compress(input, &mut ws.encoded, &mut ws.table).unwrap();
    assert!(encoded_len <= lz4::compress_bound(input.len()).unwrap());
    let encoded = &ws.encoded[..encoded_len];

    expect_valid_block(encoded, input.len());

    // Exact-size decompression must reproduce the input bit for bit. The
    // output is poisoned first so unwritten bytes cannot pass as correct.
    let output = &mut ws.decoded[..input.len()];
    output.fill(0xAA);
    assert_eq!(input.len(), lz4::decompress(encoded, output).unwrap());
    assert_eq!(input, &output[..]);

    // The exact-size contract must reject both smaller and larger buffers.
    if !input.is_empty() {
        assert_eq!(
            Err(lz4::DecompressError::OutputTooSmall),
            lz4::decompress(encoded, &mut ws.decoded[..input.len() - 1])
        );
    }
    assert_eq!(
        Err(lz4::DecompressError::OutputSizeMismatch),
        lz4::decompress(encoded, &mut ws.decoded[..input.len() + 1])
    );

    encoded_len
}

/// Structurally validate one encoded block against the LZ4 block format and
/// the stricter guarantees documented by our compressor. This deliberately
/// reimplements the format rather than reusing codec internals.
fn expect_valid_block(encoded: &[u8], raw_len: usize) {
    let mut ip: usize = 0;
    let mut op: usize = 0;
    let mut last_match_end: usize = 0;

    loop {
        // Every sequence, including the final one, starts with a token.
        assert!(ip < encoded.len());
        let token = encoded[ip];
        ip += 1;

        let mut literal_len: usize = (token >> 4) as usize;
        if literal_len == 15 {
            literal_len += read_extension(encoded, &mut ip);
        }
        assert!(encoded.len() - ip >= literal_len);
        ip += literal_len;
        op += literal_len;

        // The final sequence contains only literals and ends the block.
        if ip == encoded.len() {
            assert_eq!(raw_len, op);
            if last_match_end > 0 {
                assert!(raw_len - last_match_end >= LAST_LITERALS);
            }
            return;
        }

        assert!(encoded.len() - ip >= 2);
        let offset = u16::from_le_bytes([encoded[ip], encoded[ip + 1]]) as usize;
        ip += 2;
        assert!(offset >= 1);
        assert!(offset <= op);

        // Our compressor starts matches at least `match_find_limit` bytes
        // before the end and never lets one run into the final literals.
        assert!(op + MATCH_FIND_LIMIT <= raw_len);
        let mut match_len: usize = (token & 0x0F) as usize + 4;
        if token & 0x0F == 15 {
            match_len += read_extension(encoded, &mut ip);
        }
        op += match_len;
        assert!(op + LAST_LITERALS <= raw_len);
        last_match_end = op;
    }
}

/// Read one length extension: bytes of 255 accumulate until a terminator
/// below 255, which is included in the sum.
fn read_extension(encoded: &[u8], ip: &mut usize) -> usize {
    let mut total: usize = 0;
    loop {
        assert!(*ip < encoded.len());
        let value = encoded[*ip];
        *ip += 1;
        total += value as usize;
        if value != 255 {
            return total;
        }
    }
}

/// Decode deterministic corruptions of a valid block. Any result is
/// acceptable except memory unsafety, which Rust's bounds checks turn into
/// a failure. Mutations may still form a valid block, so output contents
/// are intentionally not asserted.
fn expect_mutation_safety(ws: &mut Workspace, random: &mut Prng, encoded_len: usize, raw_len: usize, mutations: usize) {
    let original = ws.encoded[..encoded_len].to_vec();

    for _ in 0..mutations {
        let block = &mut ws.encoded[..encoded_len];
        block.copy_from_slice(&original);

        match random.uint_less_than(4) {
            // Flip up to eight random bits.
            0 => {
                for _ in 0..random.int_range_at_most(1, 8) {
                    let at = random.uint_less_than(block.len());
                    block[at] ^= 1u8 << (random.int_u8() & 7);
                }
            }
            // Overwrite one random byte. Token and length bytes are the
            // most interesting targets and small blocks are mostly tokens.
            1 => {
                let at = random.uint_less_than(block.len());
                block[at] = random.int_u8();
            }
            // Splice random garbage over a random span.
            2 => {
                let at = random.uint_less_than(block.len());
                let span = random.int_range_at_most(1, 16).min(block.len() - at);
                random.bytes(&mut block[at..at + span]);
            }
            // Decode a random prefix of the intact block.
            _ => {}
        }

        let len = if random.boolean() {
            random.uint_at_most(block.len())
        } else {
            block.len()
        };
        let _ = lz4::decompress(&ws.encoded[..len], &mut ws.decoded[..raw_len]);
    }
}

/// Workload knobs shared by the light and exhaustive suites.
struct Budget {
    /// Upper bound and step of the contiguous small-size sweep applied to
    /// every generator.
    sweep_max: usize,
    sweep_step: usize,

    /// Number and maximum size of random-parameter inputs.
    random_inputs: usize,
    random_max: usize,

    /// Explicit match periods checked with `fillPeriodic`.
    periods: &'static [usize],
    period_len: usize,

    /// Number of corrupted decode attempts per mutation base block.
    mutations: usize,
}

impl Budget {
    fn max_input(&self) -> usize {
        self.random_max
            .max(self.period_len.max(BOUNDARY_SIZES[BOUNDARY_SIZES.len() - 1]))
    }
}

const LIGHT_BUDGET: Budget = Budget {
    sweep_max: 96,
    sweep_step: 1,
    random_inputs: 24,
    random_max: 32 * 1024,
    // One period per copy strategy: byte propagation (3), pattern words
    // (1/2/4/8), word strides (9), wide strides (17), plus the 64 KiB
    // window edge cases.
    periods: &[1, 2, 3, 4, 8, 9, 17, 65534, 65535, 65536, 65537],
    period_len: 160 * 1024,
    mutations: 64,
};

const EXHAUSTIVE_BUDGET: Budget = Budget {
    sweep_max: 2048,
    sweep_step: 1,
    random_inputs: 512,
    random_max: 512 * 1024,
    periods: &[
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 23, 24, 31, 32, 33, 48, 63, 64, 65, 127,
        128, 255, 256, 257, 4095, 4096, 32768, 65533, 65534, 65535, 65536, 65537, 65538,
    ],
    period_len: 320 * 1024,
    mutations: 4096,
};

fn run_suite(budget: &Budget, seed: u64) {
    let mut random = Prng::init(seed);
    let mut ws = Workspace::init(budget.max_input());

    // Every generator across every boundary size and the small-size sweep.
    // Small inputs hit the literal-only format edges: below the minimum
    // match sizes, around nibble limits, and around extension steps.
    for gen in Generator::ALL {
        for size in BOUNDARY_SIZES {
            gen.fill(&mut random, &mut ws.input[..size]);
            expect_codec_properties(&mut ws, size);
        }

        let mut size: usize = 0;
        while size <= budget.sweep_max {
            gen.fill(&mut random, &mut ws.input[..size]);
            expect_codec_properties(&mut ws, size);
            size += budget.sweep_step;
        }
    }

    // Random generator and size pairs, biased toward interesting sizes by
    // squaring so both tiny and large inputs appear.
    for _ in 0..budget.random_inputs {
        let gen = Generator::random(&mut random);
        let scale = random.float();
        let size = (scale * scale * budget.random_max as f64) as usize;
        gen.fill(&mut random, &mut ws.input[..size]);
        let encoded_len = expect_codec_properties(&mut ws, size);

        // Reuse a handful of these encodings as corruption bases.
        expect_mutation_safety(
            &mut ws,
            &mut random,
            encoded_len,
            size,
            budget.mutations / budget.random_inputs + 1,
        );
    }

    // Directed match periods, including the 64 KiB window edge where the
    // compressor's 16-bit position arithmetic wraps.
    for &period in budget.periods {
        fill_periodic(&mut random, &mut ws.input[..budget.period_len], period);
        expect_codec_properties(&mut ws, budget.period_len);
    }

    // Dedicated corruption run over a text-like block, plus an exhaustive
    // truncation sweep: every prefix of a valid block must decode cleanly
    // or fail cleanly.
    {
        let len = (16 * 1024).min(budget.random_max);
        Generator::Words.fill(&mut random, &mut ws.input[..len]);
        let encoded_len = expect_codec_properties(&mut ws, len);
        expect_mutation_safety(&mut ws, &mut random, encoded_len, len, budget.mutations);

        for prefix in 0..encoded_len {
            let _ = lz4::decompress(&ws.encoded[..prefix], &mut ws.decoded[..len]);
        }
    }
}

#[test]
fn lz4_differential_light() {
    run_suite(&LIGHT_BUDGET, 0x4C5A_3403);
}

#[test]
fn lz4_differential_exhaustive() {
    // Slow. Enable explicitly, ideally together with a test filter:
    //   GHOSTTY_LZ4_SLOW=1 cargo test --lib lz4_differential
    if std::env::var_os("GHOSTTY_LZ4_SLOW").is_none() {
        return;
    }

    // Several independent seeds; the suite is deterministic per seed.
    for seed in 0..4u64 {
        run_suite(&EXHAUSTIVE_BUDGET, 0x4C5A_4000 + seed);
    }
}
