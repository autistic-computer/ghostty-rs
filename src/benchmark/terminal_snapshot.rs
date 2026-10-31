//! Port of `src/benchmark/TerminalSnapshot.zig`.
//!
//! STATUS: ported
//!
//! Benchmarks the terminal binary snapshot codecs (`terminal/snapshot`).
//!
//! Encoding and decoding a snapshot are the hot paths for terminal
//! persistence and handoff, so both directions are measured against the
//! same terminal state.
//!
//! ## Input
//!
//! `--data` names a pre-generated VT byte stream (for example from
//! `ghostty-gen ascii`). The stream is fed to a terminal of the requested
//! dimensions during setup, outside the timed region. The resulting screen
//! and scrollback contents are what each step encodes or decodes.
//!
//! ## Modes
//!
//! * `noop` performs no codec work and establishes loop overhead.
//! * `encode` encodes one complete snapshot per loop into a reusable
//!   buffer. Buffer growth happens on the first iteration only.
//! * `decode` restores one complete snapshot per loop from bytes prepared
//!   during setup. The restored terminal is destroyed inside the step, so
//!   this measures the complete restore lifecycle.
//! * `decode-ready` restores only the renderable prefix per loop using the
//!   incremental decoder and stops at READY, skipping history. Compared
//!   against `decode`, this is the time-to-interactive win of applying
//!   history off the critical path.
//! * `report` encodes once and prints total and per-record-tag sizes, plus
//!   the byte offsets of READY and the first HISTORY record. It is for
//!   inspecting the wire shape, not timing comparisons. `--output` also
//!   writes the encoded snapshot to a file, for example to measure how it
//!   compresses further down the line.
//!
//! ## Kitty Graphics
//!
//! `--kitty-images` transmits that many Kitty graphics images into the
//! terminal during setup, after the input corpus, each placed at the
//! cursor and followed by enough newlines to push it into scrollback, so
//! images end up both on screen and in history. Each image is either the
//! PNG named by `--kitty-image` or a synthetic RGBA image of
//! `--kitty-width` by `--kitty-height` pixels drawn with `--kitty-pattern`.
//! Synthetic images all differ from each other, while a PNG is repeated
//! as it is, so compressors that look back far enough see repeats.
//!
//! ## Examples
//!
//! Build benchmarks in release mode:
//!
//!     cargo build --release --bin ghostty-bench --bin ghostty-gen
//!
//! Generate a deterministic corpus, then measure both directions:
//!
//!     ghostty-gen ascii | head -c 1000000 > /tmp/ascii.vt
//!     hyperfine --warmup 3 \
//!       'ghostty-bench +terminal-snapshot --mode=encode --loops=20 --data=/tmp/ascii.vt' \
//!       'ghostty-bench +terminal-snapshot --mode=decode --loops=20 --data=/tmp/ascii.vt'
//!
//! Measure a terminal with eight 512x512 textured images, then dump the
//! snapshot to see how it compresses further down the line:
//!
//!     hyperfine --warmup 3 \
//!       'ghostty-bench +terminal-snapshot --mode=encode --kitty-images=8 --kitty-width=512 --kitty-height=512' \
//!       'ghostty-bench +terminal-snapshot --mode=decode --kitty-images=8 --kitty-width=512 --kitty-height=512'
//!     ghostty-bench +terminal-snapshot --mode=report --kitty-images=8 --output=/tmp/images.snap
//!     zstd -3 -c /tmp/images.snap | wc -c

use std::any::Any;
use std::convert::Infallible;
use std::fmt;
use std::hint::black_box;
use std::io::{self as std_io, BufReader, Write as _};

use base64::Engine as _;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options;
use crate::cli::args::{cli_enum, cli_struct};
use crate::lib::compat::file as compat_file;
use crate::synthetic::DefaultPrng;
use crate::terminal::snapshot;
use crate::terminal::snapshot::io::FixedReader;
use crate::terminal::terminal::{InitError, Options as TerminalOptions, Terminal};

const LOG: &str = "terminal-snapshot-bench";

pub struct TerminalSnapshot {
    pub opts: Options,
    pub terminal: Option<Terminal>,

    /// Reused across encode steps so buffer growth is a one-time setup cost.
    pub encoded: Vec<u8>,
}

cli_struct! {
    /// Zig also keeps an `_arena` for string option ownership; Rust
    /// options own their strings.
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Select the operation performed inside the timed benchmark step.
        pub mode: Mode = Mode::Encode,

        /// Number of codec operations per benchmark step. Increase this when
        /// the state is too small for stable `hyperfine` measurements.
        pub loops: u32 = 1,

        /// The size of the terminal. This affects wrapping, page sizes, and
        /// the amount of state per encoded page.
        pub terminal_rows as "terminal-rows": u16 = 24,
        pub terminal_cols as "terminal-cols": u16 = 80,

        /// Pre-generated VT stream fed to the terminal during setup. `-` reads
        /// stdin, although a regular file is recommended so identical state can
        /// be reused across runs. When unset, the terminal is empty.
        pub data: Option<String> = None,

        /// In report mode, write the encoded snapshot to this path.
        pub output: Option<String> = None,

        /// Kitty graphics images transmitted during setup. See "Kitty
        /// Graphics" above.
        pub kitty_images as "kitty-images": u32 = 0,

        /// A PNG file to transmit for each image. When unset, a synthetic
        /// image is generated instead.
        pub kitty_image as "kitty-image": Option<String> = None,

        /// The size of each synthetic image in pixels.
        pub kitty_width as "kitty-width": u32 = 256,
        pub kitty_height as "kitty-height": u32 = 256,

        /// What each synthetic image looks like, which decides how well its
        /// pixels compress.
        pub kitty_pattern as "kitty-pattern": KittyPattern = KittyPattern::Textured,
    }
}

cli_enum! {
    pub enum KittyPattern {
        /// One color everywhere. Compresses almost completely.
        Flat => "flat",

        /// Smooth color ramps. Compresses well.
        Gradient => "gradient",

        /// Ramps with a little noise, like a photograph or a rendered
        /// screenshot. Compresses a little.
        Textured => "textured",

        /// Random bytes. Does not compress.
        Noise => "noise",
    }
}

cli_enum! {
    pub enum Mode {
        /// Establish the benchmark loop overhead.
        Noop => "noop",

        /// Encode one complete snapshot per loop.
        Encode => "encode",

        /// Restore one complete snapshot per loop, including its teardown.
        Decode => "decode",

        /// Restore one renderable READY prefix per loop, including teardown.
        DecodeReady => "decode-ready",

        /// Print encoded sizes by record tag. Not a timing benchmark.
        Report => "report",
    }
}

/// Errors from setup (Zig's inferred error set of `setupImpl`).
#[derive(Debug)]
enum SetupError {
    Init(InitError),
    Io(std_io::Error),
    FileTooBig,
    Snapshot(snapshot::io::Error),
    KittyImagesDropped,
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetupError::Init(e) => write!(f, "{e}"),
            SetupError::Io(e) => write!(f, "{e}"),
            SetupError::Snapshot(e) => write!(f, "{e}"),
            SetupError::FileTooBig => f.write_str("FileTooBig"),
            SetupError::KittyImagesDropped => f.write_str("KittyImagesDropped"),
        }
    }
}

impl From<InitError> for SetupError {
    fn from(e: InitError) -> Self {
        SetupError::Init(e)
    }
}

impl From<std_io::Error> for SetupError {
    fn from(e: std_io::Error) -> Self {
        SetupError::Io(e)
    }
}

impl From<snapshot::io::Error> for SetupError {
    fn from(e: snapshot::io::Error) -> Self {
        SetupError::Snapshot(e)
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

impl BenchmarkImpl for TerminalSnapshot {
    type Options = Options;
    type CreateError = Infallible;

    fn create(opts: Options) -> Result<Box<TerminalSnapshot>, Infallible> {
        Ok(Box::new(TerminalSnapshot {
            opts,
            terminal: None,
            encoded: Vec::new(),
        }))
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Noop => step_noop,
            Mode::Encode => step_encode,
            Mode::Decode => step_decode,
            Mode::DecodeReady => step_decode_ready,
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

impl TerminalSnapshot {
    fn setup_impl(&mut self) -> Result<(), SetupError> {
        self.terminal = None;
        self.terminal = Some(Terminal::init(TerminalOptions {
            cols: self.opts.terminal_cols,
            rows: self.opts.terminal_rows,
            max_scrollback_bytes: None,
            max_scrollback_lines: None,
            ..TerminalOptions::default()
        })?);
        let terminal = self.terminal.as_mut().unwrap();

        // Feed the input corpus through the standard VT stream.
        if let Some(data_f) = options::data_file(self.opts.data.as_deref())? {
            let mut stream = terminal.vt_stream();

            let mut r = BufReader::with_capacity(4096, data_f);

            let mut buf = [0u8; 4096];
            let result = loop {
                let n = match options::read_slice_short(&mut r, &mut buf) {
                    Ok(n) => n,
                    Err(e) => break Err(e),
                };
                if n == 0 {
                    break Ok(()); // EOF reached
                }
                stream.next_slice(&buf[..n]);
            };
            stream.deinit();
            result?;
        }

        // Kitty graphics images, after the corpus so the last ones are on
        // screen and the earlier ones are in scrollback.
        if self.opts.kitty_images > 0 {
            setup_kitty_images(&self.opts, terminal)?;
        }

        // Decode restores the same bytes every step. Encode mode also uses
        // this buffer, in which case this is its one-time growth.
        self.encoded.clear();
        snapshot::encode(
            &mut self.encoded,
            terminal,
            snapshot::EncodeOptions {
                continuation: snapshot::Continuation::Ground,
            },
        )?;
        Ok(())
    }
}

/// Transmit and place the requested Kitty graphics images.
fn setup_kitty_images(opts: &Options, terminal: &mut Terminal) -> Result<(), SetupError> {
    // A PNG is sent as it is, for every image. Synthetic images are drawn
    // once per image from a different seed, so no two are alike and a
    // compressor cannot shrink the snapshot by matching whole images.
    let png: Option<Vec<u8>> = match options::data_file(opts.kitty_image.as_deref())? {
        Some(mut f) => Some(compat_file::read_to_end_alloc(&mut f, 64 * 1024 * 1024)?),
        None => None,
    };
    let width = opts.kitty_width;
    let height = opts.kitty_height;
    let mut rgba: Option<Vec<u8>> = if png.is_none() {
        Some(vec![0u8; width as usize * height as usize * 4])
    } else {
        None
    };

    let encoder = &base64::engine::general_purpose::STANDARD;
    let raw_len = match &png {
        Some(bytes) => bytes.len(),
        None => rgba.as_ref().unwrap().len(),
    };
    let mut encoded = vec![0u8; base64::encoded_len(raw_len, true).unwrap()];

    // Each image covers a block of cells at the cursor, then newlines move
    // the cursor below it so the next image lands lower and earlier ones
    // scroll into history.
    let cols: u32 = (opts.terminal_cols as u32).min(40);
    let rows: u32 = (opts.terminal_rows as u32).min(8);

    let mut command: Vec<u8> = Vec::new();
    let mut stream = terminal.vt_stream();
    for i in 0..opts.kitty_images as u64 {
        if let Some(pixels) = rgba.as_mut() {
            draw_pattern(pixels, width, height, opts.kitty_pattern, i);
        }
        let raw: &[u8] = match &png {
            Some(bytes) => bytes,
            None => rgba.as_ref().unwrap(),
        };
        let n = encoder.encode_slice(raw, &mut encoded).unwrap();
        debug_assert_eq!(n, encoded.len());

        command.clear();
        let w = &mut command;
        let mut chunks = encoded.chunks(4096).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let more: u8 = chunks.peek().is_some() as u8;
            if first {
                write!(w, "\x1b_Ga=T,i={},c={},r={},C=1,q=2,m={},", i + 1, cols, rows, more)?;
                if png.is_some() {
                    w.extend_from_slice(b"f=100;");
                } else {
                    write!(w, "f=32,s={},v={};", width, height)?;
                }
            } else {
                write!(w, "\x1b_Gm={};", more)?;
            }
            w.extend_from_slice(chunk);
            w.extend_from_slice(b"\x1b\\");
            first = false;
        }
        for _ in 0..rows {
            w.extend_from_slice(b"\r\n");
        }
        stream.next_slice(&command);
    }
    stream.deinit();

    let storage = &terminal.screens.active.kitty_images;
    if storage.images.len() != opts.kitty_images as usize {
        log::warn!(
            target: LOG,
            "only {} of {} kitty images were stored",
            storage.images.len(),
            opts.kitty_images,
        );
        return Err(SetupError::KittyImagesDropped);
    }
    Ok(())
}

/// Fill `rgba` with one image of the pattern. `seed` varies the image.
fn draw_pattern(rgba: &mut [u8], width: u32, height: u32, pattern: KittyPattern, seed: u64) {
    let prng = DefaultPrng::init(seed);
    let random = prng.random();

    // Flat and gradient images differ by their base color.
    let base: [u8; 3] = [random.int::<8>() as u8, random.int::<8>() as u8, random.int::<8>() as u8];

    let (width, height) = (width as usize, height as usize);
    for y in 0..height {
        for x in 0..width {
            let px = &mut rgba[(y * width + x) * 4..][..4];
            match pattern {
                KittyPattern::Flat => px.copy_from_slice(&[base[0], base[1], base[2], 0xff]),
                KittyPattern::Gradient | KittyPattern::Textured => {
                    px.copy_from_slice(&[
                        ((base[0] as usize + x * 255 / width) % 256) as u8,
                        ((base[1] as usize + y * 255 / height) % 256) as u8,
                        ((base[2] as usize + (x + y) * 255 / (width + height)) % 256) as u8,
                        0xff,
                    ]);
                    if pattern == KittyPattern::Textured {
                        for c in &mut px[0..3] {
                            // Zig: `random.intRangeAtMost(i16, -8, 8)`.
                            let lo = (-8i16) as u16;
                            let hi = 8i16 as u16;
                            let noise = lo.wrapping_add(random.uint_at_most::<16>(hi.wrapping_sub(lo) as u64) as u16)
                                as i16;
                            *c = (*c as i16 + noise).clamp(0, 255) as u8;
                        }
                    }
                }
                KittyPattern::Noise => random.bytes(px),
            }
        }
    }
}

/// Build the terminal state every mode shares and, for decode mode, the
/// encoded snapshot it restores. All of this is outside the timed region.
fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalSnapshot = cast(ptr);
    self_.setup_impl().map_err(|err| {
        log::warn!(target: LOG, "failed to prepare snapshot benchmark err={}", err);
        benchmark::Error::BenchmarkFailed
    })
}

fn teardown(ptr: &mut dyn Any) {
    let self_: &mut TerminalSnapshot = cast(ptr);
    self_.terminal = None;
    self_.encoded.clear();
}

fn step_noop(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalSnapshot = cast(ptr);
    for _ in 0..self_.opts.loops {
        black_box(&self_.encoded[..]);
    }
    Ok(())
}

fn step_encode(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalSnapshot = cast(ptr);
    let terminal = self_.terminal.as_ref().unwrap();
    for _ in 0..self_.opts.loops {
        self_.encoded.clear();
        snapshot::encode(
            &mut self_.encoded,
            terminal,
            snapshot::EncodeOptions {
                continuation: snapshot::Continuation::Ground,
            },
        )
        .map_err(|err| {
            log::warn!(target: LOG, "snapshot encoding failed err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        black_box(&self_.encoded[..]);
    }
    Ok(())
}

fn step_decode(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalSnapshot = cast(ptr);
    let bytes = &self_.encoded[..];
    for _ in 0..self_.opts.loops {
        let mut reader = FixedReader::new(bytes);
        let decoded = snapshot::decode(
            &mut reader,
            &snapshot::DecodeOptions {
                max_continuation_bytes: 1024 * 1024,
                ..snapshot::DecodeOptions::default()
            },
        )
        .map_err(|err| {
            log::warn!(target: LOG, "snapshot decoding failed err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        black_box(&decoded);
        drop(decoded);
    }
    Ok(())
}

/// The renderable-prefix half of decode: everything through READY, leaving
/// all history unread. The gap between this and `step_decode` is the latency
/// class the incremental decoder removes from time-to-interactive.
fn step_decode_ready(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalSnapshot = cast(ptr);
    let bytes = &self_.encoded[..];
    for _ in 0..self_.opts.loops {
        let mut reader = FixedReader::new(bytes);
        let mut decoder = snapshot::Decoder::init(&mut reader);
        let decoded = decoder
            .ready(&snapshot::DecodeOptions {
                max_continuation_bytes: 1024 * 1024,
                ..snapshot::DecodeOptions::default()
            })
            .map_err(|err| {
                log::warn!(target: LOG, "snapshot READY decoding failed err={}", err);
                benchmark::Error::BenchmarkFailed
            })?;
        black_box(&decoded);
        drop(decoded);
    }
    Ok(())
}

/// Every named record tag in declaration order (Zig:
/// `std.enums.values(snapshot.record.Tag)`).
const TAGS: [snapshot::record::Tag; 10] = {
    use snapshot::record::Tag;
    [
        Tag::TERMINAL,
        Tag::SCREEN,
        Tag::PAGE,
        Tag::HISTORY,
        Tag::READY,
        Tag::FINISH,
        Tag::CONTINUATION,
        Tag::KITTY_IMAGE_STORAGE,
        Tag::KITTY_IMAGE_PAYLOAD,
        Tag::KITTY_IMAGE_PAGE_PLACEMENTS,
    ]
};

/// Print the encoded size grouped by record tag. This shares the encoder
/// with encode mode but deliberately makes no timing claims.
fn step_report(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    use snapshot::record::{Header, Tag};

    let self_: &mut TerminalSnapshot = cast(ptr);
    let bytes = &self_.encoded[..];

    // A record tag can hold values that `Tag` does not name, so totals are
    // kept per named tag rather than in an array over every value.
    let mut payload_totals = [0u64; TAGS.len()];
    let mut record_counts = [0u64; TAGS.len()];
    let mut record_total: u64 = 0;

    // Where the decoder can first render, and where scrollback starts
    // arriving: the end of READY and the start of the first HISTORY.
    let mut ready_end: Option<usize> = None;
    let mut history_start: Option<usize> = None;

    let mut offset: usize = snapshot::envelope::ENCODED_LEN;
    while offset + Header::LEN <= bytes.len() {
        let tag_raw = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let payload_len = u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
        let tag = Tag(tag_raw);
        let Some(index) = TAGS.iter().position(|&t| t == tag) else {
            log::warn!(target: LOG, "unknown record tag {}", tag_raw);
            return Err(benchmark::Error::BenchmarkFailed);
        };
        payload_totals[index] += payload_len as u64;
        record_counts[index] += 1;
        record_total += 1;
        offset += Header::LEN + payload_len;
        match tag {
            Tag::READY => ready_end = Some(offset),
            Tag::HISTORY => {
                if history_start.is_none() {
                    history_start = Some(offset - Header::LEN - payload_len);
                }
            }
            _ => {}
        }
    }

    for ((tag, payload), records) in TAGS.iter().zip(payload_totals).zip(record_counts) {
        eprintln!(
            "terminal-snapshot tag={} records={} payload={}",
            tag.name().unwrap(),
            records,
            payload,
        );
    }
    eprintln!(
        "terminal-snapshot total records={} framing={} encoded={}",
        record_total,
        record_total * Header::LEN as u64 + snapshot::envelope::ENCODED_LEN as u64,
        bytes.len(),
    );
    eprintln!(
        "terminal-snapshot ready_end={} history_start={}",
        ready_end.unwrap_or(0),
        history_start.unwrap_or(0),
    );

    if let Some(path) = &self_.opts.output {
        std::fs::write(path, bytes).map_err(|err| {
            log::warn!(target: LOG, "failed to write snapshot to {} err={}", path, err);
            benchmark::Error::BenchmarkFailed
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;
    use crate::terminal::build_options;

    fn run(opts: Options) {
        let mut impl_ = TerminalSnapshot::create(opts).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }

    #[test]
    fn terminal_snapshot() {
        run(Options::default());
    }

    #[test]
    fn terminal_snapshot_decode_round_trip() {
        run(Options {
            mode: Mode::Decode,
            terminal_rows: 4,
            terminal_cols: 8,
            ..Options::default()
        });
    }

    #[test]
    fn terminal_snapshot_report() {
        run(Options {
            mode: Mode::Report,
            terminal_rows: 4,
            terminal_cols: 8,
            ..Options::default()
        });
    }

    #[test]
    fn terminal_snapshot_kitty_images_round_trip() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }
        run(Options {
            mode: Mode::Decode,
            terminal_rows: 4,
            terminal_cols: 8,
            kitty_images: 3,
            kitty_width: 4,
            kitty_height: 4,
            ..Options::default()
        });
    }

    #[test]
    fn terminal_snapshot_decode_ready_prefix() {
        run(Options {
            mode: Mode::DecodeReady,
            terminal_rows: 4,
            terminal_cols: 8,
            ..Options::default()
        });
    }
}
