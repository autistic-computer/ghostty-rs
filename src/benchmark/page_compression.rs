//! Port of `src/benchmark/PageCompression.zig`.
//!
//! STATUS: ported
//!
//! Benchmarks raw LZ4 compression and decompression on page-sized byte
//! buffers.
//!
//! This benchmark is intentionally independent of terminal page ownership and
//! lifecycle. It treats its input as opaque bytes and calls only the standalone
//! LZ4 block codec. In particular, it does not compress pages owned by a live
//! terminal or measure the scheduling and state transitions used by automatic
//! scrollback compression.
//!
//! ## Input
//!
//! `--data` names a pre-generated raw byte corpus. The corpus is divided into
//! `--page-size` byte chunks, with a final short chunk retained when the file
//! size is not an exact multiple. The default page size is 400 KiB, matching a
//! standard terminal page in ReleaseFast builds on the current target.
//!
//! A raw dump of actual page backing memory is the most representative input:
//! it includes cells, rows, styles, graphemes, hyperlinks, allocator metadata,
//! and unused capacity exactly as the codec would see them. Keep such corpora
//! outside the repository and reuse the same file when comparing branches.
//! Arbitrary files are accepted too, but their compression ratios should not be
//! interpreted as terminal scrollback ratios.
//!
//! ## Modes
//!
//! * `noop` walks the input chunks without invoking the codec. This measures the
//!   benchmark loop's minimum overhead.
//! * `compress` compresses every input chunk into a reusable output buffer.
//! * `store` additionally copies each useful result into an exact-sized
//!   allocation. It retains the most recent `--retained-pages` blocks so that
//!   allocation, eviction, and allocator reuse resemble bounded scrollback.
//! * `decompress` prepares compressed blocks during setup, then decompresses
//!   every block into a reusable output buffer.
//! * `report` compresses each chunk once and prints raw and encoded sizes. It is
//!   for inspecting ratios, not timing comparisons.
//!
//! Dataset loading, output allocation, and preparation of blocks for
//! decompression happen in `setup` and are outside `Benchmark`'s timed region.
//! The `compress` and `decompress` steps perform no allocation. `store`
//! deliberately performs encoded-block allocations and frees inside the timed
//! step; only its reusable workspace and ring metadata are prepared in setup.
//! `hyperfine` still measures full process lifetime, so use `--loops` to
//! amortize setup and teardown when comparing small corpora.
//!
//! ## Examples
//!
//! Build benchmarks in release mode:
//!
//!     cargo build --release --bin ghostty-bench
//!
//! Inspect the compression ratio of a page corpus:
//!
//!     ghostty-bench +page-compression --mode=report --data=/tmp/pages.raw
//!
//! Measure the cost of exact encoded storage relative to the codec alone:
//!
//!     hyperfine --warmup 3 \
//!       'ghostty-bench +page-compression --mode=compress --loops=100 --data=/tmp/pages.raw' \
//!       'ghostty-bench +page-compression --mode=store --loops=100 --data=/tmp/pages.raw'

use std::any::Any;
use std::convert::Infallible;
use std::fmt;
use std::hint::black_box;
use std::io;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options;
use crate::cli::args::{cli_enum, cli_struct};
use crate::lib::compat::file as compat_file;
use crate::terminal::compress::lz4;
use crate::terminal::compress::Page as CompressedPage;

const LOG: &str = "page-compression-bench";

/// Prevent a malformed or accidentally enormous corpus from consuming
/// unbounded memory during benchmark setup.
const MAX_DATA_SIZE: usize = 64 * 1024 * 1024;

pub struct PageCompression {
    pub opts: Options,

    /// Complete contents of the input corpus. Individual pages are slices into
    /// this allocation, so it remains alive until teardown.
    pub data: Vec<u8>,

    /// Compressed blocks prepared during setup for `decompress` mode.
    encoded: Vec<Encoded>,

    /// Exact encoded allocations retained by `store` mode. None entries have not
    /// been reached yet; once full, `stored_next` identifies the oldest block.
    pub stored: Vec<Option<Vec<u8>>>,
    pub stored_next: usize,

    /// Reused by compression and report modes. Its length is the compression
    /// bound of the largest input chunk.
    pub compression_output: Vec<u8>,

    /// Reused by decompression mode. Its length is at least one input chunk.
    pub decompression_output: Vec<u8>,

    /// Fixed 16 KiB scratch table required by the compressor.
    table: lz4::HashTable,
}

cli_struct! {
    /// Zig also keeps an `_arena` for string option ownership; Rust
    /// options own their strings.
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Select the operation performed inside the timed benchmark step.
        pub mode: Mode = Mode::Compress,

        /// Repeat the complete corpus this many times per benchmark step. Increase
        /// this when the corpus is too small for stable `hyperfine` measurements.
        pub loops: u32 = 1,

        /// Number of bytes treated as one independent LZ4 block. Real page dumps
        /// should use the exact backing-memory size of the pages being measured.
        pub page_size as "page-size": usize = 400 * 1024,

        /// Number of exact encoded allocations retained by `store` mode before
        /// the oldest allocation is freed and replaced. The default approximates
        /// a 10 MB scrollback composed of standard 400 KiB terminal pages.
        pub retained_pages as "retained-pages": usize = 25,

        /// Pre-generated input corpus. `-` reads stdin, although a regular file is
        /// recommended so identical bytes can be reused across benchmark runs.
        /// When unset, all modes are no-ops.
        pub data: Option<String> = None,
    }
}

cli_enum! {
    pub enum Mode {
        /// Walk page boundaries and establish the benchmark loop overhead.
        Noop => "noop",

        /// Compress each raw page into a reusable compression-bound buffer.
        Compress => "compress",

        /// Compress and retain exact-sized encoded blocks in a bounded ring.
        Store => "store",

        /// Decompress blocks prepared before the timed region.
        Decompress => "decompress",

        /// Print per-page and aggregate encoded sizes. Not a timing benchmark.
        Report => "report",
    }
}

struct Encoded {
    /// Exact compressed block bytes.
    bytes: Vec<u8>,

    /// Exact output length expected by the raw block decoder.
    raw_len: usize,
}

/// Errors while preparing the benchmark data (Zig's inferred error set
/// of `setupData`).
#[derive(Debug)]
enum SetupError {
    InvalidLoops,
    InvalidPageSize,
    InvalidRetainedPages,
    RoundTripMismatch,
    Io(io::Error),
    FileTooBig,
    Compress(lz4::CompressError),
    Decompress(lz4::DecompressError),
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetupError::Io(e) => write!(f, "{e}"),
            SetupError::Compress(e) => write!(f, "{e}"),
            SetupError::Decompress(e) => write!(f, "{e}"),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl From<io::Error> for SetupError {
    fn from(e: io::Error) -> Self {
        SetupError::Io(e)
    }
}

impl From<compat_file::ReadToEndAllocError> for SetupError {
    fn from(e: compat_file::ReadToEndAllocError) -> Self {
        match e {
            compat_file::ReadToEndAllocError::Io(e) => SetupError::Io(e),
            compat_file::ReadToEndAllocError::FileTooBig => SetupError::FileTooBig,
        }
    }
}

impl From<lz4::CompressError> for SetupError {
    fn from(e: lz4::CompressError) -> Self {
        SetupError::Compress(e)
    }
}

impl From<lz4::DecompressError> for SetupError {
    fn from(e: lz4::DecompressError) -> Self {
        SetupError::Decompress(e)
    }
}

impl BenchmarkImpl for PageCompression {
    type Options = Options;
    type CreateError = Infallible;

    /// Allocate benchmark state. Input data is intentionally loaded later by
    /// `setup` so construction is cheap and follows the other benchmarks.
    fn create(opts: Options) -> Result<Box<PageCompression>, Infallible> {
        Ok(Box::new(PageCompression {
            opts,
            data: Vec::new(),
            encoded: Vec::new(),
            stored: Vec::new(),
            stored_next: 0,
            compression_output: Vec::new(),
            decompression_output: Vec::new(),
            // SAFETY: the table is plain integers (Zig: `undefined`).
            table: unsafe { std::mem::zeroed() },
        }))
    }

    /// Select one operation for the benchmark harness to time.
    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Noop => step_noop,
            Mode::Compress => step_compress,
            Mode::Store => step_store,
            Mode::Decompress => step_decompress,
            Mode::Report => step_report,
        };
        Benchmark::init(
            self,
            VTable {
                step_fn,
                setup_fn: Some(setup),
                teardown_fn: Some(teardown),
            },
        )
    }
}

impl PageCompression {
    fn setup_data(&mut self) -> Result<(), SetupError> {
        if self.opts.loops == 0 {
            return Err(SetupError::InvalidLoops);
        }
        if self.opts.page_size == 0 {
            return Err(SetupError::InvalidPageSize);
        }
        if self.opts.mode == Mode::Store && self.opts.retained_pages == 0 {
            return Err(SetupError::InvalidRetainedPages);
        }

        let Some(mut data_file) = options::data_file(self.opts.data.as_deref())? else {
            return Ok(());
        };

        self.data = compat_file::read_to_end_alloc(&mut data_file, MAX_DATA_SIZE)?;
        drop(data_file);
        let result = self.setup_buffers();
        if result.is_err() {
            self.data = Vec::new();
        }
        result
    }

    fn setup_buffers(&mut self) -> Result<(), SetupError> {
        if self.data.is_empty() {
            return Ok(());
        }
        if self.opts.mode == Mode::Noop {
            return Ok(());
        }

        let largest_page = self.opts.page_size.min(self.data.len());
        self.compression_output = vec![0; lz4::compress_bound(largest_page)?];

        let result = (|| {
            if self.opts.mode == Mode::Store {
                self.prepare_stored();
            }
            if self.opts.mode == Mode::Decompress {
                self.prepare_encoded()?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.compression_output = Vec::new();
        }
        result
    }

    /// Allocate only the ring metadata for `store` mode. The encoded blocks which
    /// populate it are intentionally allocated by the timed benchmark step.
    fn prepare_stored(&mut self) {
        self.stored = vec![None; self.opts.retained_pages];
        self.stored_next = 0;
    }

    /// Precompress every input page and verify one decode before benchmarking.
    /// This catches corpus or codec problems before the timer starts.
    fn prepare_encoded(&mut self) -> Result<(), SetupError> {
        self.decompression_output = vec![0; self.opts.page_size.min(self.data.len())];

        let result = (|| {
            let mut it = pages(&self.data, &self.opts);
            while let Some(page) = it.next() {
                let encoded_len = lz4::compress(page, &mut self.compression_output, &mut self.table)?;
                let encoded = self.compression_output[..encoded_len].to_vec();
                self.encoded.push(Encoded {
                    bytes: encoded,
                    raw_len: page.len(),
                });
            }

            let mut page_it = pages(&self.data, &self.opts);
            for block in &self.encoded {
                let page = page_it.next().unwrap();
                let output = &mut self.decompression_output[..block.raw_len];
                let _ = lz4::decompress(&block.bytes, output)?;
                if page != &*output {
                    return Err(SetupError::RoundTripMismatch);
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.decompression_output = Vec::new();
        }
        result
    }

    /// Release everything created by setup. This is shared by teardown and
    /// drop so errors and direct unit-test use remain leak-free.
    fn clear_prepared_data(&mut self) {
        self.encoded = Vec::new();
        self.stored = Vec::new();
        self.stored_next = 0;
        self.compression_output = Vec::new();
        self.decompression_output = Vec::new();
        self.data = Vec::new();
    }

}

/// Iterate fixed-size page chunks without allocating an index table.
///
/// Zig: `self.pages()`. This takes the data and options rather than the
/// whole benchmark so callers can write the reusable output buffers
/// (disjoint fields) while iterating.
fn pages<'a>(data: &'a [u8], opts: &Options) -> PageIterator<'a> {
    PageIterator {
        data,
        page_size: opts.page_size,
        offset: 0,
    }
}

impl Drop for PageCompression {
    /// Release allocations retained across benchmark steps.
    fn drop(&mut self) {
        self.clear_prepared_data();
    }
}

/// Load and partition the input corpus. For decompression mode this also
/// creates the encoded blocks, keeping compression outside the timed region.
fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut PageCompression = cast(ptr);
    debug_assert!(self_.data.is_empty());
    debug_assert!(self_.encoded.is_empty());

    self_.setup_data().map_err(|err| {
        log::warn!(target: LOG, "failed to prepare page compression benchmark err={}", err);
        benchmark::Error::BenchmarkFailed
    })
}

fn teardown(ptr: &mut dyn Any) {
    let self_: &mut PageCompression = cast(ptr);
    self_.clear_prepared_data();
}

/// Baseline mode: traverse exactly the same page boundaries as compression
/// without invoking the codec.
fn step_noop(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut PageCompression = cast(ptr);
    for _ in 0..self_.opts.loops {
        let mut it = pages(&self_.data, &self_.opts);
        while let Some(page) = it.next() {
            black_box(page);
        }
    }
    Ok(())
}

/// Compress all pages into one reusable output buffer. Only the returned
/// encoded length is consumed because retaining output pages would measure
/// allocation and ownership rather than codec throughput.
fn step_compress(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut PageCompression = cast(ptr);
    for _ in 0..self_.opts.loops {
        let mut it = pages(&self_.data, &self_.opts);
        while let Some(page) = it.next() {
            let encoded_len = lz4::compress(page, &mut self_.compression_output, &mut self_.table).map_err(|err| {
                log::warn!(target: LOG, "page compression failed err={}", err);
                benchmark::Error::BenchmarkFailed
            })?;
            black_box(encoded_len);
        }
    }
    Ok(())
}

/// Compress pages and retain their exact encoded allocations in a bounded
/// ring. This models the part of `compress.Page.init` which differs from the
/// allocation-free codec benchmark: copying the result, allocating its
/// persistent storage, and freeing an old block when scrollback is full.
fn step_store(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut PageCompression = cast(ptr);
    if self_.data.is_empty() {
        return Ok(());
    }
    debug_assert!(!self_.stored.is_empty());

    for _ in 0..self_.opts.loops {
        let mut it = pages(&self_.data, &self_.opts);
        while let Some(page) = it.next() {
            let output_len = CompressedPage::required_scratch(page.len()).map_err(|err| {
                log::warn!(target: LOG, "failed to size stored page output err={}", err);
                benchmark::Error::BenchmarkFailed
            })?;
            if output_len == 0 {
                continue;
            }

            let encoded_len =
                match lz4::compress(page, &mut self_.compression_output[..output_len], &mut self_.table) {
                    Ok(n) => n,
                    // This is the same profitability outcome as
                    // compress.Page.init: a block which exceeds the useful output
                    // limit remains resident and consumes no encoded allocation.
                    Err(lz4::CompressError::OutputTooSmall) => continue,
                    Err(err @ lz4::CompressError::InputTooLarge) => {
                        log::warn!(target: LOG, "stored page compression failed err={}", err);
                        return Err(benchmark::Error::BenchmarkFailed);
                    }
                };

            let encoded = self_.compression_output[..encoded_len].to_vec();

            let slot = &mut self_.stored[self_.stored_next];
            // Assigning frees the previous block, if any.
            *slot = Some(encoded);
            self_.stored_next = (self_.stored_next + 1) % self_.stored.len();
            black_box(&self_.stored);
        }
    }
    Ok(())
}

/// Decompress blocks prepared by setup. The output allocation is reused so
/// this measures only decoding and the required memory writes.
fn step_decompress(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut PageCompression = cast(ptr);
    for _ in 0..self_.opts.loops {
        for block in &self_.encoded {
            let output = &mut self_.decompression_output[..block.raw_len];
            let _ = lz4::decompress(&block.bytes, output).map_err(|err| {
                log::warn!(target: LOG, "page decompression failed err={}", err);
                benchmark::Error::BenchmarkFailed
            })?;
            black_box(output);
        }
    }
    Ok(())
}

/// Print size information for evaluating compression ratio. This shares the
/// input and codec paths with compression mode but deliberately makes no
/// timing claims.
fn step_report(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut PageCompression = cast(ptr);
    if self_.data.is_empty() {
        return Ok(());
    }

    let mut page_index: usize = 0;
    let mut raw_total: usize = 0;
    let mut encoded_total: usize = 0;
    let mut it = pages(&self_.data, &self_.opts);
    while let Some(page) = it.next() {
        let encoded_len = lz4::compress(page, &mut self_.compression_output, &mut self_.table).map_err(|err| {
            log::warn!(target: LOG, "page compression report failed err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        raw_total += page.len();
        encoded_total += encoded_len;
        eprintln!(
            "page-compression page={} raw={} encoded={} ratio={:.2}%",
            page_index,
            page.len(),
            encoded_len,
            percentage(encoded_len, page.len()),
        );
        page_index += 1;
    }

    eprintln!(
        "page-compression total pages={} raw={} encoded={} ratio={:.2}% workspace={} output_bound={}",
        page_index,
        raw_total,
        encoded_total,
        percentage(encoded_total, raw_total),
        std::mem::size_of::<lz4::HashTable>(),
        self_.compression_output.len(),
    );
    Ok(())
}

struct PageIterator<'a> {
    data: &'a [u8],
    page_size: usize,
    offset: usize,
}

impl<'a> PageIterator<'a> {
    fn next(&mut self) -> Option<&'a [u8]> {
        if self.offset >= self.data.len() {
            return None;
        }
        let len = self.page_size.min(self.data.len() - self.offset);
        let end = self.offset + len;
        let result = &self.data[self.offset..end];
        self.offset = end;
        Some(result)
    }
}

fn percentage(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 * 100.0 / whole as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn page_compression() {
        let mut impl_ = PageCompression::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }

    #[test]
    fn page_compression_store_retains_exact_encoded_allocations() {
        const PAGE_SIZE: usize = 1024;
        let mut impl_ = PageCompression::create(Options {
            mode: Mode::Store,
            page_size: PAGE_SIZE,
            retained_pages: 2,
            ..Options::default()
        })
        .unwrap();

        impl_.data = vec![0; 3 * PAGE_SIZE];
        impl_.compression_output = vec![0; lz4::compress_bound(PAGE_SIZE).unwrap()];
        impl_.prepare_stored();

        step_store(&mut *impl_).unwrap();
        assert_eq!(1usize, impl_.stored_next);
        for block in &impl_.stored {
            let encoded = block.as_ref().unwrap();
            assert!(encoded.len() < PAGE_SIZE);

            let mut decoded = [0xAAu8; PAGE_SIZE];
            let _ = lz4::decompress(encoded, &mut decoded).unwrap();
            assert!(decoded.iter().all(|&b| b == 0));
        }
    }
}
