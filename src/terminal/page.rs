//! Port of `src/terminal/page.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - `Row` and `Cell` are `packed struct(u64)` in Zig. They are `u64`
//!   newtypes here with getters/setters that preserve the exact bit
//!   layout (see [`row_fields`] and [`cell_fields`]).
//! - Pointers into page memory (Zig `*Row`, `*Cell`, `[]Cell`) are raw
//!   pointers when returned by accessors ([`Page::get_row`],
//!   [`Page::get_row_and_cell`], [`Page::get_cells`]). Methods that operate
//!   on a row or cell take `&mut Row`/`&mut Cell` references; the caller is
//!   responsible for creating them from valid pointers into this page.
//! - The backing memory is a [`PageMemory`] (pointer + length), the Rust
//!   counterpart of Zig's `[]align(page_size_min) u8`.
//! - `clone_from`/`clone_row_from`/`clone_partial_row_from` take the source
//!   page as `*const Page` because the source may be `self` (Zig compares
//!   `other == self`); they are `unsafe fn` for that reason.
//! - `verify_integrity` does not take an allocator.
//! - `Mask` takes the field mask as a const generic built with
//!   [`field_mask`] over [`BitField`] descriptors instead of field-name
//!   strings.

use core::fmt;
use core::marker::PhantomData;
use core::ptr::NonNull;
use std::collections::HashMap;

use crate::terminal::bitmap_allocator::BitmapAllocator;
use crate::terminal::build_options;
use crate::terminal::color;
use crate::terminal::hash_map::{self, AutoOffsetHashMap};
use crate::terminal::hyperlink;
use crate::terminal::kitty::graphics_unicode;
use crate::terminal::mem::PAGE_SIZE_MIN;
use crate::terminal::ref_counted_set::AddError;
use crate::terminal::size::{self, align_forward, get_offset, AllocError, Base, Offset, OffsetBuf, OffsetSlice};
use crate::terminal::style::{self, Style};

const LOG_TARGET: &str = "page";

type StyleId = style::Id;
type StyleSet = style::Set;

/// The backing memory of a page: a page-aligned, zero-initialized block
/// (Zig: `[]align(std.heap.page_size_min) u8`).
///
/// This is a plain (pointer, length) pair; it does not own the memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageMemory {
    ptr: NonNull<u8>,
    len: usize,
}

impl PageMemory {
    /// Create from a raw pointer and length.
    ///
    /// # Safety
    ///
    /// `ptr` must be non-null and point to `len` bytes valid for reads and
    /// writes for as long as the memory is used.
    pub unsafe fn from_raw_parts(ptr: *mut u8, len: usize) -> PageMemory {
        PageMemory {
            ptr: NonNull::new(ptr).expect("page memory must not be null"),
            len,
        }
    }

    #[inline]
    pub fn ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// A prefix of this memory (Zig: `buf[0..len]`).
    #[inline]
    pub fn prefix(&self, len: usize) -> PageMemory {
        assert!(len <= self.len);
        PageMemory { ptr: self.ptr, len }
    }

    /// # Safety
    ///
    /// The memory must be valid and not mutated while the slice lives.
    #[inline]
    pub unsafe fn as_slice<'a>(&self) -> &'a [u8] {
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// # Safety
    ///
    /// The memory must be valid and not otherwise accessed while the slice
    /// lives.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn as_mut_slice<'a>(&self) -> &'a mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Base for PageMemory {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl Base for &PageMemory {
    #[inline]
    fn base_ptr(self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

/// Page-aligned allocator used for terminal page backing memory. Pages
/// require page-aligned, zeroed memory obtained directly from the OS
/// (not the Zig allocator) because the allocation fast-path is
/// performance-critical and the OS guarantees zeroed pages.
#[cfg(not(windows))]
type PageAlloc = AllocPosix;
#[cfg(windows)]
type PageAlloc = AllocWindows;

/// Allocate page-aligned, zeroed backing memory using mmap with
/// MAP_PRIVATE | MAP_ANONYMOUS which guarantees zeroed pages.
#[cfg(not(windows))]
struct AllocPosix;

#[cfg(not(windows))]
impl AllocPosix {
    fn alloc(n: usize) -> Result<PageMemory, AllocError> {
        // SAFETY: anonymous private mapping; no file descriptor involved.
        let ptr = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                n,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED || ptr.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        // SAFETY: mmap returned a valid mapping of `n` bytes.
        Ok(unsafe { PageMemory::from_raw_parts(ptr.cast(), n) })
    }

    fn free(mem: PageMemory) {
        // SAFETY: `mem` was returned by `alloc` (or covers a whole mapping).
        unsafe { libc::munmap(mem.ptr().cast(), mem.len()) };
    }
}

/// Allocate page-aligned, zeroed backing memory using VirtualAlloc with
/// MEM_COMMIT | MEM_RESERVE which guarantees zeroed pages.
#[cfg(windows)]
struct AllocWindows;

#[cfg(windows)]
impl AllocWindows {
    fn alloc(n: usize) -> Result<PageMemory, AllocError> {
        use crate::os::windows;
        // SAFETY: plain reserve+commit of fresh memory.
        let addr = unsafe {
            windows::exp::kernel32::VirtualAlloc(
                core::ptr::null_mut(),
                n,
                windows::MEM_COMMIT | windows::MEM_RESERVE,
                windows::PAGE_READWRITE,
            )
        };
        if addr.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        // SAFETY: VirtualAlloc returned `n` committed bytes.
        Ok(unsafe { PageMemory::from_raw_parts(addr.cast(), n) })
    }

    fn free(mem: PageMemory) {
        use crate::os::windows;
        // SAFETY: `mem` was returned by `alloc`.
        unsafe { windows::exp::kernel32::VirtualFree(mem.ptr().cast(), 0, windows::MEM_RELEASE) };
    }
}

/// The allocator to use for multi-codepoint grapheme data. We use
/// a chunk size of 4 codepoints. It'd be best to set this empirically
/// but it is currently set based on vibes. My thinking around 4 codepoints
/// is that most skin-tone emoji are <= 4 codepoints, letter combiners
/// are usually <= 4 codepoints, and 4 codepoints is a nice power of two
/// for alignment.
const GRAPHEME_CHUNK_LEN: usize = 4;
const GRAPHEME_CHUNK: usize = GRAPHEME_CHUNK_LEN * core::mem::size_of::<u32>();
pub const GRAPHEME_MAX_LEN: usize = 64;
type GraphemeAlloc = BitmapAllocator<GRAPHEME_CHUNK>;
const GRAPHEME_COUNT_DEFAULT: usize = GraphemeAlloc::BITMAP_BIT_SIZE;
pub const GRAPHEME_BYTES_DEFAULT: usize = GRAPHEME_COUNT_DEFAULT * GRAPHEME_CHUNK;
type GraphemeMap = AutoOffsetHashMap<Offset<Cell>, OffsetSlice<u32>, { hash_map::DEFAULT_MAX_LOAD_PERCENTAGE }>;

/// The allocator used for shared utf8-encoded strings within a page.
/// Note the chunk size below is the minimum size of a single allocation
/// and requires a single bit of metadata in our bitmap allocator. Therefore
/// it should be tuned carefully (too small and we waste metadata, too large
/// and we have fragmentation). We can probably use a better allocation
/// strategy in the future.
///
/// At the time of writing this, the strings table is only used for OSC8
/// IDs and URIs. IDs are usually short and URIs are usually longer. I chose
/// 32 bytes as a compromise between these two since it represents single
/// domain links quite well and is not too wasteful for short IDs. We can
/// continue to tune this as we see how it's used.
const STRING_CHUNK_LEN: usize = 32;
const STRING_CHUNK: usize = STRING_CHUNK_LEN * core::mem::size_of::<u8>();
type StringAlloc = BitmapAllocator<STRING_CHUNK>;
const STRING_COUNT_DEFAULT: usize = StringAlloc::BITMAP_BIT_SIZE;
pub const STRING_BYTES_DEFAULT: usize = STRING_COUNT_DEFAULT * STRING_CHUNK;

/// Default number of hyperlinks we support.
///
/// The cell multiplier is the number of cells per hyperlink entry that
/// we support. A hyperlink can be longer than this multiplier; the multiplier
/// just sets the total capacity to simplify adjustable size metrics.
const HYPERLINK_COUNT_DEFAULT: usize = 4;
const HYPERLINK_BYTES_DEFAULT: usize = HYPERLINK_COUNT_DEFAULT * core::mem::size_of::<hyperlink::SetItem>();
const HYPERLINK_CELL_MULTIPLIER: usize = 16;

/// `std.atomic.cache_line` for the current target.
const CACHE_LINE: usize = if cfg!(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "powerpc64"
)) {
    128
} else if cfg!(any(
    target_arch = "arm",
    target_arch = "mips",
    target_arch = "mips64",
    target_arch = "riscv32",
    target_arch = "riscv64"
)) {
    32
} else if cfg!(target_arch = "s390x") {
    256
} else {
    64
};

const fn max_usize(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

const fn min_usize(a: usize, b: usize) -> usize {
    if a < b {
        a
    } else {
        b
    }
}

/// The alignment of the start of a page's cell array. Align it to a cache
/// line so that row cells always start on a cache line. This avoids
/// a scenario where cells in every row always start mid-cache line and
/// straddle an extra.
const CELLS_ALIGN: usize = max_usize(
    core::mem::align_of::<Cell>(),
    // Cap it at page size for freestanding targets.
    min_usize(CACHE_LINE, PAGE_SIZE_MIN),
);

/// The integrity errors that `verify_integrity` can detect
/// (Zig: `Page.IntegrityError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityError {
    ZeroRowCount,
    ZeroColCount,
    UnmarkedGraphemeRow,
    MissingGraphemeData,
    InvalidGraphemeCount,
    UnmarkedGraphemeCell,
    MissingStyle,
    UnmarkedStyleRow,
    MismatchedStyleRef,
    InvalidStyleCount,
    MissingHyperlinkData,
    MismatchedHyperlinkRef,
    UnmarkedHyperlinkCell,
    UnmarkedHyperlinkRow,
    InvalidSpacerTailLocation,
    InvalidSpacerHeadLocation,
    UnwrappedSpacerHead,
}

impl fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for IntegrityError {}

/// (Zig: `Page.StyleSetError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleSetError {
    StyleSetOutOfMemory,
    StyleSetNeedsRehash,
}

/// (Zig: `Page.HyperlinkError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HyperlinkError {
    StringAllocOutOfMemory,
    HyperlinkSetOutOfMemory,
    HyperlinkSetNeedsRehash,
    HyperlinkMapOutOfMemory,
}

/// (Zig: `Page.GraphemeError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphemeError {
    GraphemeMapOutOfMemory,
    GraphemeAllocOutOfMemory,
}

/// `StyleSetError || HyperlinkError || GraphemeError`
/// (Zig: `Page.CloneFromError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneFromError {
    StyleSetOutOfMemory,
    StyleSetNeedsRehash,
    StringAllocOutOfMemory,
    HyperlinkSetOutOfMemory,
    HyperlinkSetNeedsRehash,
    HyperlinkMapOutOfMemory,
    GraphemeMapOutOfMemory,
    GraphemeAllocOutOfMemory,
}

/// (Zig: `Page.InsertHyperlinkError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertHyperlinkError {
    /// string_alloc errors
    StringsOutOfMemory,

    /// hyperlink_set errors
    SetOutOfMemory,
    SetNeedsRehash,
}

macro_rules! error_boilerplate {
    ($($t:ty),*) => { $(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(self, f)
            }
        }
        impl std::error::Error for $t {}
    )* };
}
error_boilerplate!(StyleSetError, HyperlinkError, GraphemeError, CloneFromError, InsertHyperlinkError);

impl From<StyleSetError> for CloneFromError {
    fn from(e: StyleSetError) -> Self {
        match e {
            StyleSetError::StyleSetOutOfMemory => CloneFromError::StyleSetOutOfMemory,
            StyleSetError::StyleSetNeedsRehash => CloneFromError::StyleSetNeedsRehash,
        }
    }
}

impl From<HyperlinkError> for CloneFromError {
    fn from(e: HyperlinkError) -> Self {
        match e {
            HyperlinkError::StringAllocOutOfMemory => CloneFromError::StringAllocOutOfMemory,
            HyperlinkError::HyperlinkSetOutOfMemory => CloneFromError::HyperlinkSetOutOfMemory,
            HyperlinkError::HyperlinkSetNeedsRehash => CloneFromError::HyperlinkSetNeedsRehash,
            HyperlinkError::HyperlinkMapOutOfMemory => CloneFromError::HyperlinkMapOutOfMemory,
        }
    }
}

impl From<GraphemeError> for CloneFromError {
    fn from(e: GraphemeError) -> Self {
        match e {
            GraphemeError::GraphemeMapOutOfMemory => CloneFromError::GraphemeMapOutOfMemory,
            GraphemeError::GraphemeAllocOutOfMemory => CloneFromError::GraphemeAllocOutOfMemory,
        }
    }
}

/// The row and cell for an x/y position (Zig: the anonymous struct
/// returned by `getRowAndCell`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowAndCell {
    pub row: *mut Row,
    pub cell: *mut Cell,
}

/// A page represents a specific section of terminal screen. The primary
/// idea of a page is that it is a fully self-contained unit that can be
/// serialized, copied, etc. as a convenient way to represent a section
/// of the screen.
///
/// This property is useful for renderers which want to copy just the pages
/// for the visible portion of the screen, or for infinite scrollback where
/// we may want to serialize and store pages that are sufficiently far
/// away from the current viewport.
///
/// Pages are always backed by a single contiguous block of memory that is
/// aligned on a page boundary. This makes it easy and fast to copy pages
/// around. Within the contiguous block of memory, the contents of a page are
/// thoughtfully laid out to optimize primarily for terminal IO (VT streams)
/// and to minimize memory usage.
#[derive(Debug, Clone, Copy)]
pub struct Page {
    /// The backing memory for the page. A page is always made up of a
    /// a single contiguous block of memory that is aligned on a page
    /// boundary and is a multiple of the system page size.
    pub memory: PageMemory,

    /// The array of rows in the page. The rows are always in row order
    /// (i.e. index 0 is the top row, index 1 is the row below that, etc.)
    pub rows: Offset<Row>,

    /// The array of cells in the page. The cells are NOT in row order,
    /// but they are in column order. To determine the mapping of cells
    /// to row, you must use the `rows` field. From the pointer to the
    /// first column, all cells in that row are laid out in column order.
    pub cells: Offset<Cell>,

    /// Set to true when an operation is performed that dirties all rows in
    /// the page. See `Row.dirty` for more information on dirty tracking.
    ///
    /// NOTE: A value of false does NOT indicate that
    ///       the page has no dirty rows in it, only
    ///       that no full-page-dirtying operations
    ///       have occurred since it was last cleared.
    pub dirty: bool,

    /// The string allocator for this page used for shared utf-8 encoded
    /// strings. Liveness of strings and memory management is deferred to
    /// the individual use case.
    pub string_alloc: StringAlloc,

    /// The multi-codepoint grapheme data for this page. This is where
    /// any cell that has more than one codepoint will be stored. This is
    /// relatively rare (typically only emoji) so this defaults to a very small
    /// size and we force page realloc when it grows.
    pub grapheme_alloc: GraphemeAlloc,

    /// The mapping of cell to grapheme data. The exact mapping is the
    /// cell offset to the grapheme data offset. Therefore, whenever a
    /// cell is moved (i.e. `erase`) then the grapheme data must be updated.
    /// Grapheme data is relatively rare so this is considered a slow
    /// path.
    pub grapheme_map: GraphemeMap,

    /// The available set of styles in use on this page.
    pub styles: StyleSet,

    /// The structures used for tracking hyperlinks within the page.
    /// The map maps cell offsets to hyperlink IDs and the IDs are in
    /// the ref counted set. The strings within the hyperlink structures
    /// are allocated in the string allocator.
    pub hyperlink_map: hyperlink::Map,
    pub hyperlink_set: hyperlink::Set,

    /// The current dimensions of the page. The capacity may be larger
    /// than this. This allows us to allocate a larger page than necessary
    /// and also to resize a page smaller without reallocating.
    pub size: Size,

    /// The capacity of this page. This is the full size of the backing
    /// memory and is fixed at page creation time.
    pub capacity: Capacity,

    /// If this is nonzero then verifyIntegrity will do nothing. This is
    /// only used with runtime safety enabled.
    pub pause_integrity_checks: usize,
}

// The alignment of our members. We want to ensure that the page
// alignment is always divisible by this.
const _: () = assert!(
    PAGE_SIZE_MIN % max_usize(max_usize(core::mem::align_of::<Row>(), CELLS_ALIGN), MetaLayout::ALIGNMENT) == 0
);

impl Page {
    /// Initialize a new page, allocating the required backing memory.
    /// The size of the initialized page defaults to the full capacity.
    ///
    /// The backing memory is always allocated using mmap directly.
    /// You cannot use custom allocators with this structure because
    /// it is critical to performance that we use mmap.
    #[inline]
    pub fn init(cap: Capacity) -> Result<Page, AllocError> {
        let l = Page::layout(cap);

        // We allocate page-aligned zeroed memory directly to avoid
        // allocator overhead (small but meaningful for this path). Both
        // mmap (POSIX) and VirtualAlloc (Windows) guarantee zeroed pages,
        // which is a critical property for us.
        debug_assert!(l.total_size % PAGE_SIZE_MIN == 0);
        let backing = PageAlloc::alloc(l.total_size)?;

        let buf = OffsetBuf::init(backing);
        // SAFETY: fresh, zeroed, page-aligned memory of `l.total_size` bytes.
        Ok(unsafe { Page::init_buf(buf, l) })
    }

    /// Initialize a new page using the given backing memory.
    /// It is up to the caller to not call deinit on these pages.
    ///
    /// The backing memory must be zero-filled. A page treats zero as the
    /// empty state everywhere: cells are blank when zero, and every
    /// metadata member initializes from zeroed memory without writing
    /// anything. Only the row headers are written, so the OS pages behind
    /// everything else stay untouched until first use.
    ///
    /// # Safety
    ///
    /// `buf.start()` must point to `l.total_size` zero-filled bytes, aligned
    /// to the page size, valid for reads and writes for the page's lifetime.
    #[inline]
    pub unsafe fn init_buf(buf: OffsetBuf, l: Layout) -> Page {
        let cap = l.capacity;

        // A page must always have at least one row.
        assert!(cap.rows > 0);

        let rows = buf.member::<Row>(l.rows_start);
        let cells = buf.member::<Cell>(l.cells_start);

        // We need to go through and initialize all the rows so that
        // they point to a valid offset into the cells, since the rows
        // zero-initialized aren't valid.
        let cells_ptr = cells.ptr(buf);
        let rows_ptr = rows.ptr(buf);
        for y in 0..cap.rows as usize {
            let start = y * cap.cols as usize;
            // SAFETY: rows/cells lie within the buffer per the layout.
            unsafe {
                rows_ptr
                    .add(y)
                    .write(Row::init(get_offset(buf, cells_ptr.add(start).cast_const())));
            }
        }

        // SAFETY: the caller guarantees the buffer.
        unsafe {
            Page {
                memory: PageMemory::from_raw_parts(buf.start(), l.total_size),
                rows,
                cells,
                styles: StyleSet::init_assume_zeroed(buf.add(l.styles_start), l.styles_layout, style::SetContext),
                string_alloc: StringAlloc::init_assume_zeroed(buf.add(l.string_alloc_start), l.string_alloc_layout),
                grapheme_alloc: GraphemeAlloc::init_assume_zeroed(
                    buf.add(l.grapheme_alloc_start),
                    l.grapheme_alloc_layout,
                ),
                grapheme_map: GraphemeMap::init_assume_zeroed(buf.add(l.grapheme_map_start), l.grapheme_map_layout),
                hyperlink_map: hyperlink::Map::init_assume_zeroed(
                    buf.add(l.hyperlink_map_start),
                    l.hyperlink_map_layout,
                ),
                hyperlink_set: hyperlink::Set::init_assume_zeroed(
                    buf.add(l.hyperlink_set_start),
                    l.hyperlink_set_layout,
                    hyperlink::SetContext::default(),
                ),
                size: Size {
                    cols: cap.cols,
                    rows: cap.rows,
                },
                capacity: cap,
                dirty: false,
                pause_integrity_checks: 0,
            }
        }
    }

    /// Deinitialize the page, freeing any backing memory. Do NOT call
    /// this if you allocated the backing memory yourself (i.e. you used
    /// initBuf).
    #[inline]
    pub fn deinit(&mut self) {
        PageAlloc::free(self.memory);
    }

    /// Reinitialize the page with the same capacity.
    #[inline]
    pub fn reinit(&mut self) {
        // We zero the page memory as u64 instead of u8 because
        // we can and it's empirically quite a bit faster.
        // SAFETY: the page memory is valid and 8-byte aligned.
        unsafe {
            core::ptr::write_bytes(self.memory.ptr().cast::<u64>(), 0, self.memory.len() / 8);
            *self = Page::init_buf(OffsetBuf::init(self.memory), Page::layout(self.capacity));
        }
    }

    /// Temporarily pause integrity checks. This is useful when you are
    /// doing a lot of operations that would trigger integrity check
    /// violations but you know the page will end up in a consistent state.
    #[inline]
    pub fn pause_integrity_checks(&mut self, v: bool) {
        if build_options::SLOW_RUNTIME_SAFETY {
            if v {
                self.pause_integrity_checks += 1;
            } else {
                self.pause_integrity_checks -= 1;
            }
        }
    }

    /// A helper that can be used to assert the integrity of the page when
    /// runtime safety is enabled. This is a no-op when runtime safety is
    /// disabled.
    #[inline]
    #[track_caller]
    pub fn assert_integrity(&self) {
        if build_options::SLOW_RUNTIME_SAFETY {
            if let Err(err) = self.verify_integrity() {
                log::error!(target: LOG_TARGET, "page integrity violation, crashing. err={}", err);
                panic!("page integrity violation");
            }
        }
    }

    /// Verifies the integrity of the page data. This is not fast,
    /// but it is useful for assertions, deserialization, etc.
    ///
    /// Integrity errors are also logged as warnings.
    pub fn verify_integrity(&self) -> Result<(), IntegrityError> {
        // Some things that seem like we should check but do not:
        //
        // - We do not check that the style ref count is exact, only that
        //   it is at least what we see. We do this because some fast paths
        //   trim rows without clearing data.
        // - We do not check that styles seen is exactly the same as the
        //   styles count in the page for the same reason as above.
        // - We only check that we saw less graphemes than the total memory
        //   used for the same reason as styles above.
        //

        if build_options::SLOW_RUNTIME_SAFETY && self.pause_integrity_checks > 0 {
            return Ok(());
        }

        if self.size.rows == 0 {
            log::warn!(target: LOG_TARGET, "page integrity violation zero row count");
            return Err(IntegrityError::ZeroRowCount);
        }
        if self.size.cols == 0 {
            log::warn!(target: LOG_TARGET, "page integrity violation zero col count");
            return Err(IntegrityError::ZeroColCount);
        }

        let mut graphemes_seen: usize = 0;
        let mut styles_seen: HashMap<StyleId, usize> = HashMap::new();
        let mut hyperlinks_seen: HashMap<hyperlink::Id, usize> = HashMap::new();

        let grapheme_count = self.grapheme_count();

        let mem = self.memory;
        let cols = self.size.cols as usize;
        // SAFETY: rows/cells are within the page memory.
        let rows = unsafe { core::slice::from_raw_parts(self.rows.ptr(mem), self.size.rows as usize) };
        for (y, row) in rows.iter().enumerate() {
            let graphemes_start = graphemes_seen;
            // SAFETY: each row's cells are within the page memory.
            let cells = unsafe { core::slice::from_raw_parts(row.cells().ptr(mem), cols) };
            for (x, cell) in cells.iter().enumerate() {
                if cell.has_grapheme() {
                    // If a cell has grapheme data, it must be present in
                    // the grapheme map.
                    if self.lookup_grapheme(cell).is_none() {
                        log::warn!(
                            target: LOG_TARGET,
                            "page integrity violation y={} x={} grapheme data missing",
                            y,
                            x
                        );
                        return Err(IntegrityError::MissingGraphemeData);
                    }

                    graphemes_seen += 1;
                } else if grapheme_count > 0 {
                    // It should not have grapheme data if it isn't marked.
                    // The grapheme_count check above is just an optimization
                    // to speed up integrity checks.
                    if self.lookup_grapheme(cell).is_some() {
                        log::warn!(
                            target: LOG_TARGET,
                            "page integrity violation y={} x={} cell not marked as grapheme",
                            y,
                            x
                        );
                        return Err(IntegrityError::UnmarkedGraphemeCell);
                    }
                }

                if cell.style_id() != style::DEFAULT_ID {
                    // If a cell has a style, it must be present in the styles
                    // set. Accessing it with `get` asserts that.
                    // SAFETY: `mem` is the page base.
                    let _ = unsafe { self.styles.get(mem, cell.style_id()) };

                    if !row.styled() {
                        log::warn!(
                            target: LOG_TARGET,
                            "page integrity violation y={} x={} row not marked as styled",
                            y,
                            x
                        );
                        return Err(IntegrityError::UnmarkedStyleRow);
                    }

                    *styles_seen.entry(cell.style_id()).or_insert(0) += 1;
                }

                if cell.hyperlink() {
                    let Some(id) = self.lookup_hyperlink(cell) else {
                        log::warn!(
                            target: LOG_TARGET,
                            "page integrity violation y={} x={} hyperlink data missing",
                            y,
                            x
                        );
                        return Err(IntegrityError::MissingHyperlinkData);
                    };

                    if !row.hyperlink() {
                        log::warn!(
                            target: LOG_TARGET,
                            "page integrity violation y={} x={} row not marked as hyperlink",
                            y,
                            x
                        );
                        return Err(IntegrityError::UnmarkedHyperlinkRow);
                    }

                    *hyperlinks_seen.entry(id).or_insert(0) += 1;

                    // Hyperlink ID should be valid. This just straight crashes
                    // if this fails due to assertions.
                    // SAFETY: `mem` is the page base.
                    let _ = unsafe { self.hyperlink_set.get(mem, id) };
                } else {
                    // It should not have hyperlink data if it isn't marked
                    if self.lookup_hyperlink(cell).is_some() {
                        log::warn!(
                            target: LOG_TARGET,
                            "page integrity violation y={} x={} cell not marked as hyperlink",
                            y,
                            x
                        );
                        return Err(IntegrityError::UnmarkedHyperlinkCell);
                    }
                }

                match cell.wide() {
                    Wide::Narrow => {}
                    Wide::Wide => {}

                    Wide::SpacerTail => {
                        // Spacer tails can't be at the start because they follow
                        // a wide char.
                        if x == 0 {
                            log::warn!(
                                target: LOG_TARGET,
                                "page integrity violation y={} x={} spacer tail at start",
                                y,
                                x
                            );
                            return Err(IntegrityError::InvalidSpacerTailLocation);
                        }

                        // Spacer tails must follow a wide char
                        let prev = cells[x - 1];
                        if prev.wide() != Wide::Wide {
                            log::warn!(
                                target: LOG_TARGET,
                                "page integrity violation y={} x={} spacer tail not following wide",
                                y,
                                x
                            );
                            return Err(IntegrityError::InvalidSpacerTailLocation);
                        }
                    }

                    Wide::SpacerHead => {
                        // Spacer heads must be at the end
                        if x != cols - 1 {
                            log::warn!(
                                target: LOG_TARGET,
                                "page integrity violation y={} x={} spacer head not at end",
                                y,
                                x
                            );
                            return Err(IntegrityError::InvalidSpacerHeadLocation);
                        }

                        // The row must be wrapped
                        if !row.wrap() {
                            log::warn!(target: LOG_TARGET, "page integrity violation y={} spacer head not wrapped", y);
                            return Err(IntegrityError::UnwrappedSpacerHead);
                        }
                    }
                }
            }

            // Check row grapheme data
            if graphemes_seen > graphemes_start {
                // If a cell in a row has grapheme data, the row must
                // be marked as having grapheme data.
                if !row.grapheme() {
                    log::warn!(
                        target: LOG_TARGET,
                        "page integrity violation y={} grapheme data but row not marked",
                        y
                    );
                    return Err(IntegrityError::UnmarkedGraphemeRow);
                }
            }
        }

        // Our graphemes seen should exactly match the grapheme count
        if graphemes_seen > self.grapheme_count() {
            log::warn!(
                target: LOG_TARGET,
                "page integrity violation grapheme count mismatch expected={} actual={}",
                graphemes_seen,
                self.grapheme_count()
            );
            return Err(IntegrityError::InvalidGraphemeCount);
        }

        // Verify all our styles have the correct ref count.
        for (&id, &expected) in styles_seen.iter() {
            // SAFETY: `mem` is the page base.
            let ref_count = unsafe { self.styles.ref_count(mem, id) } as usize;
            if ref_count < expected {
                log::warn!(
                    target: LOG_TARGET,
                    "page integrity violation style ref count mismatch id={} expected={} actual={}",
                    id,
                    expected,
                    ref_count
                );
                return Err(IntegrityError::MismatchedStyleRef);
            }
        }

        // Verify all our hyperlinks have the correct ref count.
        for (&id, &expected) in hyperlinks_seen.iter() {
            // SAFETY: `mem` is the page base.
            let ref_count = unsafe { self.hyperlink_set.ref_count(mem, id) } as usize;
            if ref_count < expected {
                log::warn!(
                    target: LOG_TARGET,
                    "page integrity violation hyperlink ref count mismatch id={} expected={} actual={}",
                    id,
                    expected,
                    ref_count
                );
                return Err(IntegrityError::MismatchedHyperlinkRef);
            }
        }

        // Verify there are no zombie styles, that is, styles in the
        // set with ref counts > 0, which are not present in the page.
        {
            // SAFETY: the set's table/items arrays are within the page memory.
            let styles_table =
                unsafe { core::slice::from_raw_parts(self.styles.table.ptr(mem), self.styles.layout.table_cap) };
            let styles_items =
                unsafe { core::slice::from_raw_parts(self.styles.items.ptr(mem), self.styles.layout.cap) };

            let mut zombies: usize = 0;

            for &id in styles_table {
                if id == 0 {
                    continue;
                }
                let item = &styles_items[id as usize];
                if item.meta.r#ref == 0 {
                    continue;
                }

                let expected = styles_seen.get(&id).copied().unwrap_or(0);
                if expected > 0 {
                    continue;
                }

                if item.meta.r#ref as usize > expected {
                    zombies += 1;
                }
            }

            // NOTE: This is currently disabled because @qwerasd says that
            // certain fast paths can cause this but its okay.
            // Just 1 zombie style might be the cursor style, so ignore it.
            // if zombies > 1 {
            //     log::warn!(target: LOG_TARGET, "page integrity violation zombie styles count={}", zombies);
            //     return Err(IntegrityError::ZombieStyles);
            // }
            let _ = zombies;
        }

        Ok(())
    }

    /// Clone the contents of this page. This will allocate new memory
    /// using the page allocator. If you want to manage memory manually,
    /// use clone_buf.
    #[inline]
    pub fn clone(&self) -> Result<Page, AllocError> {
        let backing = PageAlloc::alloc(self.memory.len())?;
        // SAFETY: `backing` is fresh memory of the same size.
        Ok(unsafe { self.clone_buf(backing) })
    }

    /// Clone the entire contents of this page.
    ///
    /// The buffer must be at least the size of self.memory.
    ///
    /// # Safety
    ///
    /// `buf` must be valid, page aligned, writable memory that doesn't
    /// overlap this page's memory.
    #[inline]
    pub unsafe fn clone_buf(&self, buf: PageMemory) -> Page {
        assert!(buf.len() >= self.memory.len());

        // The entire concept behind a page is that everything is stored
        // as offsets so we can do a simple linear copy of the backing
        // memory and copy all the offsets and everything will work.
        let mut result = *self;
        result.memory = buf.prefix(self.memory.len());

        // This is a memcpy. We may want to investigate if there are
        // faster ways to do this (i.e. copy-on-write tricks) but I suspect
        // they'll be slower. I haven't experimented though.
        // SAFETY: caller contract.
        unsafe {
            core::ptr::copy_nonoverlapping(self.memory.ptr(), result.memory.ptr(), self.memory.len());
        }

        result
    }

    /// Compute the exact capacity required to store a range of rows from
    /// this page.
    ///
    /// The returned capacity will have the same number of columns as this
    /// page and the number of rows equal to the range given. The returned
    /// capacity is by definition strictly less than or equal to this
    /// page's capacity, so the layout is guaranteed to succeed.
    ///
    /// Preconditions:
    /// - Range must be at least 1 row
    /// - Start and end must be valid for this page
    pub fn exact_row_capacity(&self, y_start: usize, y_end: usize) -> Capacity {
        assert!(y_start < y_end);
        assert!(y_end <= self.size.rows as usize);

        // Track unique IDs using a bitset. Both style IDs and hyperlink IDs
        // are CellCountInt (u16), so we reuse this set for both to save
        // stack memory (~8KB instead of ~16KB).
        struct CellCountSet([u64; (u16::MAX as usize + 1) / 64]);
        impl CellCountSet {
            const EMPTY: CellCountSet = CellCountSet([0; (u16::MAX as usize + 1) / 64]);
            fn set(&mut self, i: u16) {
                self.0[i as usize / 64] |= 1u64 << (i % 64);
            }
            fn is_set(&self, i: u16) -> bool {
                self.0[i as usize / 64] & (1u64 << (i % 64)) != 0
            }
            fn count(&self) -> usize {
                self.0.iter().map(|w| w.count_ones() as usize).sum()
            }
        }
        const _: () = assert!(core::mem::size_of::<size::StyleCountInt>() == core::mem::size_of::<size::CellCountInt>());
        const _: () =
            assert!(core::mem::size_of::<size::HyperlinkCountInt>() == core::mem::size_of::<size::CellCountInt>());

        // Accumulators
        let mut id_set = CellCountSet::EMPTY;
        let mut grapheme_bytes: usize = 0;
        let mut string_bytes: usize = 0;

        let mem = self.memory;
        let cols = self.size.cols as usize;

        // First pass: count styles and grapheme bytes
        // SAFETY: rows are within the page memory.
        let rows = unsafe { core::slice::from_raw_parts(self.rows.ptr(mem).add(y_start), y_end - y_start) };
        for row in rows {
            // SAFETY: each row's cells are within the page memory.
            let cells = unsafe { core::slice::from_raw_parts(row.cells().ptr(mem), cols) };
            for cell in cells {
                if cell.style_id() != style::DEFAULT_ID {
                    id_set.set(cell.style_id());
                }

                if cell.has_grapheme() {
                    if let Some(cps) = self.lookup_grapheme(cell) {
                        grapheme_bytes += GraphemeAlloc::bytes_required::<u32>(cps.len());
                    }
                }
            }
        }
        let styles_cap = StyleSet::capacity_for_count(id_set.count());

        // Second pass: count hyperlinks and string bytes
        // We count both unique hyperlinks (for hyperlink_set) and total
        // hyperlink cells (for hyperlink_map capacity).
        id_set = CellCountSet::EMPTY;
        let mut hyperlink_cells: usize = 0;
        for row in rows {
            // SAFETY: each row's cells are within the page memory.
            let cells = unsafe { core::slice::from_raw_parts(row.cells().ptr(mem), cols) };
            for cell in cells {
                if cell.hyperlink() {
                    hyperlink_cells += 1;
                    if let Some(id) = self.lookup_hyperlink(cell) {
                        // Only count each unique hyperlink once for set sizing
                        if !id_set.is_set(id) {
                            id_set.set(id);

                            // Get the hyperlink entry to compute string bytes
                            // SAFETY: `mem` is the page base.
                            let entry = unsafe { *self.hyperlink_set.get(mem, id) };
                            string_bytes += StringAlloc::bytes_required::<u8>(entry.uri.len);

                            match entry.id {
                                hyperlink::PageEntryId::Implicit(_) => {}
                                hyperlink::PageEntryId::Explicit(slice) => {
                                    string_bytes += StringAlloc::bytes_required::<u8>(slice.len);
                                }
                            }
                        }
                    }
                }
            }
        }

        // layout() requests `hyperlink_count * hyperlink_cell_multiplier`
        // usable map entries. The map layout adds load-factor headroom and
        // rounds the raw slot count to a power of two. We need enough
        // hyperlink_bytes for that requested entry count to accommodate all
        // hyperlink cells. This is unit tested.
        let hyperlink_cap = {
            let hyperlink_count = id_set.count();
            let hyperlink_set_cap = hyperlink::Set::capacity_for_count(hyperlink_count);
            let hyperlink_map_min = hyperlink_cells.div_ceil(HYPERLINK_CELL_MULTIPLIER);
            hyperlink_set_cap.max(hyperlink_map_min)
        };

        // All the casts below are safe because we should have a
        // capacity strictly less than or equal to this page's capacity.
        Capacity {
            cols: self.size.cols,
            rows: (y_end - y_start) as size::CellCountInt,
            styles: styles_cap as size::StyleCountInt,
            grapheme_bytes: grapheme_bytes as size::GraphemeBytesInt,
            hyperlink_bytes: (hyperlink_cap * core::mem::size_of::<hyperlink::SetItem>()) as size::HyperlinkCountInt,
            string_bytes: string_bytes as size::StringBytesInt,
        }
    }

    /// Clone the contents of another page into this page. The capacities
    /// can be different, but the size of the other page must fit into
    /// this page.
    ///
    /// The y_start and y_end parameters allow you to clone only a portion
    /// of the other page. This is useful for splitting a page into two
    /// or more pages.
    ///
    /// The column count of this page will always be the same as this page.
    /// If the other page has more columns, the extra columns will be
    /// truncated. If the other page has fewer columns, the extra columns
    /// will be zeroed.
    ///
    /// # Safety
    ///
    /// `other` must point to a live page; it may be `self`.
    #[inline]
    pub unsafe fn clone_from(&mut self, other: *const Page, y_start: usize, y_end: usize) -> Result<(), CloneFromError> {
        let other_page: Page = if core::ptr::eq(other, self) { *self } else { unsafe { *other } };
        assert!(y_start <= y_end);
        assert!(y_end <= other_page.size.rows as usize);
        assert!(y_end - y_start <= self.size.rows as usize);

        let other_rows = other_page.rows.ptr(other_page.memory);
        let rows = self.rows.ptr(self.memory);
        for i in 0..(y_end - y_start) {
            // SAFETY: indices are within both pages' rows.
            unsafe {
                let dst_row = &mut *rows.add(i);
                let src_row = &*other_rows.add(y_start + i);
                self.clone_row_from(other, dst_row, src_row)?;
            }
        }

        // We should remain consistent
        self.assert_integrity();
        Ok(())
    }

    /// Clone a single row from another page into this page.
    ///
    /// # Safety
    ///
    /// `other` must point to a live page; it may be `self`. `src_row` must
    /// be a row of `other` and `dst_row` a row of this page.
    #[inline]
    pub unsafe fn clone_row_from(&mut self, other: *const Page, dst_row: &mut Row, src_row: &Row) -> Result<(), CloneFromError> {
        let cols = self.size.cols as usize;
        unsafe { self.clone_partial_row_from(other, dst_row, src_row, 0, cols) }
    }

    /// Clone a single row from another page into this page, supporting
    /// partial copy. cloneRowFrom calls this.
    ///
    /// # Safety
    ///
    /// `other` must point to a live page; it may be `self`. `src_row` must
    /// be a row of `other` and `dst_row` a (different) row of this page.
    pub unsafe fn clone_partial_row_from(
        &mut self,
        other: *const Page,
        dst_row: &mut Row,
        src_row: &Row,
        x_start: usize,
        x_end_req: usize,
    ) -> Result<(), CloneFromError> {
        // This whole operation breaks integrity until the end.
        self.pause_integrity_checks(true);
        let result = unsafe { self.clone_partial_row_from_inner(other, dst_row, src_row, x_start, x_end_req) };
        self.pause_integrity_checks(false);
        self.assert_integrity();
        result
    }

    unsafe fn clone_partial_row_from_inner(
        &mut self,
        other_ptr: *const Page,
        dst_row: &mut Row,
        src_row: &Row,
        x_start: usize,
        x_end_req: usize,
    ) -> Result<(), CloneFromError> {
        // Whether we're cloning within the same page. The other page's
        // metadata is snapshotted (it's all offsets into its memory, which
        // is unchanged by copying the struct).
        let same = core::ptr::eq(other_ptr, self);
        let other: Page = if same { *self } else { unsafe { *other_ptr } };

        let cell_len = (self.size.cols.min(other.size.cols)) as usize;
        let x_end = x_end_req.min(cell_len);
        assert!(x_start <= x_end);
        let len = x_end - x_start;
        // SAFETY: the rows' cells are within their pages' memory.
        let other_cells: *mut Cell = unsafe { src_row.cells().ptr(other.memory).add(x_start) };
        let cells: *mut Cell = unsafe { dst_row.cells().ptr(self.memory).add(x_start) };

        // If our destination has styles or graphemes then we need to
        // clear some state. This will free up the managed memory as well.
        if dst_row.managed_memory() {
            self.clear_cells(dst_row, x_start, x_end);
        }

        // Copy all the row metadata but keep our cells offset
        *dst_row = {
            let mut copy = *src_row;

            // If we're not copying the full row then we want to preserve
            // some original state from our dst row.
            if len < self.size.cols as usize {
                copy.set_wrap(dst_row.wrap());
                copy.set_wrap_continuation(dst_row.wrap_continuation());
                copy.set_grapheme(dst_row.grapheme());
                copy.set_hyperlink(dst_row.hyperlink());
                copy.set_styled(dst_row.styled());
                copy.set_dirty(copy.dirty() | dst_row.dirty());
            }

            // Our cell offset remains the same
            copy.set_cells(dst_row.cells());

            copy
        };

        // If we have no managed memory in the source, then we can just
        // copy it directly.
        if !src_row.managed_memory() {
            // This is an integrity check: if the row claims it doesn't
            // have managed memory then all cells must also not have
            // managed memory.
            if build_options::SLOW_RUNTIME_SAFETY {
                for i in 0..len {
                    // SAFETY: within the source row.
                    let cell = unsafe { *other_cells.add(i) };
                    assert!(!cell.has_grapheme());
                    assert!(!cell.hyperlink());
                    assert!(cell.style_id() == style::DEFAULT_ID);
                }
            }

            // SAFETY: both ranges are `len` cells within their rows.
            unsafe { core::ptr::copy(other_cells, cells, len) };
        } else {
            // We have managed memory, so we have to do a slower copy to
            // get all of that right.
            for i in 0..len {
                // SAFETY: within the rows; src and dst are different cells.
                let (src_cell, dst_cell) = unsafe { (&*other_cells.add(i), &mut *cells.add(i)) };
                *dst_cell = *src_cell;

                // Reset any managed memory markers on the cell so that we don't
                // hit an integrity check if we have to return an error because
                // the page can't fit the new memory.
                dst_cell.set_hyperlink(false);
                dst_cell.set_style_id(style::DEFAULT_ID);
                if dst_cell.content_tag() == ContentTag::CodepointGrapheme {
                    dst_cell.set_content_tag(ContentTag::Codepoint);
                }

                if src_cell.has_grapheme() {
                    // Copy the grapheme codepoints
                    let cps = other.lookup_grapheme(src_cell).unwrap();

                    // Safe to use setGraphemes because we cleared all
                    // managed memory for our destination cell range.
                    self.set_graphemes(dst_row, dst_cell, cps)?;
                }
                if src_cell.hyperlink() {
                    let id = other.lookup_hyperlink(src_cell).unwrap();

                    if same {
                        // Fast-path: same page we can add with the same id.
                        // SAFETY: `self.memory` is the page base.
                        unsafe { self.hyperlink_set.use_(self.memory, id) };
                        self.set_hyperlink(dst_row, dst_cell, id)?;
                    } else {
                        // Slow-path: get the hyperlink from the other page,
                        // add it, and migrate.

                        // If our page can't support an additional cell with
                        // a hyperlink then we have to return an error.
                        if self.hyperlink_count() >= self.hyperlink_capacity() {
                            // The hyperlink map capacity needs to be increased.
                            return Err(CloneFromError::HyperlinkMapOutOfMemory);
                        }

                        // SAFETY: `other.memory` is the other page's base.
                        let other_link: hyperlink::PageEntry = unsafe { *other.hyperlink_set.get(other.memory, id) };
                        let dst_id = 'dst_id: {
                            // First check if the link already exists in our page,
                            // and increment its refcount if so, since we're about
                            // to use it.
                            let ctx = hyperlink::SetContext {
                                page: self as *mut Page,
                                src_page: &other as *const Page,
                            };
                            // SAFETY: all access goes through `ctx.page` (self).
                            let found = unsafe {
                                (*ctx.page)
                                    .hyperlink_set
                                    .lookup_context((*ctx.page).memory, other_link, &ctx)
                            };
                            if let Some(i) = found {
                                // SAFETY: `self.memory` is the page base.
                                unsafe { self.hyperlink_set.use_(self.memory, i) };
                                break 'dst_id i;
                            }

                            // If we don't have this link in our page yet then
                            // we need to clone it over and add it to our set.

                            // Clone the link.
                            let dst_link = match other_link.dupe(&other, self) {
                                Ok(v) => v,
                                // The string alloc capacity needs to be increased.
                                Err(AllocError::OutOfMemory) => return Err(CloneFromError::StringAllocOutOfMemory),
                            };

                            // Add it, preferring to use the same ID as the other
                            // page, since this *probably* speeds up full-page
                            // clones.
                            //
                            // TODO(qwerasd): verify the assumption that `addWithId`
                            // is ever actually useful, I think it may not be.
                            let ctx = hyperlink::SetContext::new(self as *mut Page);
                            // SAFETY: all access goes through `ctx.page` (self).
                            let added = unsafe {
                                (*ctx.page)
                                    .hyperlink_set
                                    .add_with_id_context((*ctx.page).memory, dst_link, id, &ctx)
                            };
                            match added {
                                Ok(v) => v.unwrap_or(id),
                                // The hyperlink set capacity needs to be increased.
                                Err(AddError::OutOfMemory) => return Err(CloneFromError::HyperlinkSetOutOfMemory),
                                // The hyperlink set needs to be rehashed.
                                Err(AddError::NeedsRehash) => return Err(CloneFromError::HyperlinkSetNeedsRehash),
                            }
                        };

                        self.set_hyperlink(dst_row, dst_cell, dst_id)?;
                    }
                }
                if src_cell.style_id() != style::DEFAULT_ID {
                    dst_row.set_styled(true);

                    if same {
                        // If it's the same page we don't have to worry about
                        // copying the style, we can use the style ID directly.
                        dst_cell.set_style_id(src_cell.style_id());
                        // SAFETY: `self.memory` is the page base.
                        unsafe { self.styles.use_(self.memory, dst_cell.style_id()) };
                    } else {
                        // Slow path: Get the style from the other
                        // page and add it to this page's style set.
                        // SAFETY: `other.memory` is the other page's base.
                        let other_style: Style = unsafe { *other.styles.get(other.memory, src_cell.style_id()) };
                        // SAFETY: `self.memory` is the page base.
                        let added = unsafe { self.styles.add_with_id(self.memory, other_style, src_cell.style_id()) };
                        dst_cell.set_style_id(match added {
                            Ok(v) => v.unwrap_or(src_cell.style_id()),
                            // The style set capacity needs to be increased.
                            Err(AddError::OutOfMemory) => return Err(CloneFromError::StyleSetOutOfMemory),
                            // The style set needs to be rehashed.
                            Err(AddError::NeedsRehash) => return Err(CloneFromError::StyleSetNeedsRehash),
                        });
                    }
                }
                if build_options::KITTY_GRAPHICS && src_cell.codepoint() == graphics_unicode::PLACEHOLDER {
                    dst_row.set_kitty_virtual_placeholder(true);
                }
            }
        }

        // If we are growing columns, then we need to ensure spacer heads
        // are cleared.
        if self.size.cols > other.size.cols {
            // SAFETY: the destination range is `len` cells within the row.
            let cells = unsafe { core::slice::from_raw_parts_mut(cells, len) };
            let last = &mut cells[other.size.cols as usize - 1];
            if last.wide() == Wide::SpacerHead {
                last.set_wide(Wide::Narrow);
            }
        }

        Ok(())
    }

    /// Get a single row. y must be valid.
    #[inline]
    pub fn get_row(&self, y: usize) -> *mut Row {
        assert!(y < self.size.rows as usize);
        // SAFETY: y is within the rows array.
        unsafe { self.rows.ptr(self.memory).add(y) }
    }

    /// Get the cells for a row.
    #[inline]
    pub fn get_cells(&self, row: &Row) -> *mut [Cell] {
        if build_options::SLOW_RUNTIME_SAFETY {
            let rows = self.rows.ptr(self.memory);
            let cells = self.cells.ptr(self.memory);
            let row_addr = row as *const Row as usize;
            assert!(row_addr >= rows as usize);
            assert!(row_addr < cells as usize);
        }

        let cells = row.cells().ptr(self.memory);
        core::ptr::slice_from_raw_parts_mut(cells, self.size.cols as usize)
    }

    /// Get the row and cell for the given X/Y within this page.
    #[inline]
    pub fn get_row_and_cell(&self, x: usize, y: usize) -> RowAndCell {
        assert!(y < self.size.rows as usize);
        assert!(x < self.size.cols as usize);

        let rows = self.rows.ptr(self.memory);
        // SAFETY: x/y are within the page.
        unsafe {
            let row = rows.add(y);
            let cell = (*row).cells().ptr(self.memory).add(x);
            RowAndCell { row, cell }
        }
    }

    /// Move a cell from one location to another. This will replace the
    /// previous contents with a blank cell. Because this is a move, this
    /// doesn't allocate and can't fail.
    pub fn move_cells(&mut self, src_row: &mut Row, src_left: usize, dst_row: &mut Row, dst_left: usize, len: usize) {
        let mem = self.memory;
        debug_assert!(src_left + len <= self.size.cols as usize);
        debug_assert!(dst_left + len <= self.size.cols as usize);
        // SAFETY: the ranges are within the rows.
        let src_cells: *mut Cell = unsafe { src_row.cells().ptr(mem).add(src_left) };
        let dst_cells: *mut Cell = unsafe { dst_row.cells().ptr(mem).add(dst_left) };

        // Clear our destination now matter what
        self.clear_cells(dst_row, dst_left, dst_left + len);

        // If src has no managed memory, this is very fast.
        if !src_row.managed_memory() {
            // SAFETY: both ranges are `len` cells within the page.
            unsafe { core::ptr::copy(src_cells, dst_cells, len) };
        } else {
            // Source has graphemes or hyperlinks...
            for i in 0..len {
                // SAFETY: within the rows; different rows so no aliasing.
                let (src, dst) = unsafe { (&mut *src_cells.add(i), &mut *dst_cells.add(i)) };
                *dst = *src;
                if src.has_grapheme() {
                    // Required for moveGrapheme assertions
                    dst.set_content_tag(ContentTag::Codepoint);
                    self.move_grapheme(src, dst);
                    src.set_content_tag(ContentTag::Codepoint);
                    dst.set_content_tag(ContentTag::CodepointGrapheme);
                    dst_row.set_grapheme(true);
                }
                if src.hyperlink() {
                    dst.set_hyperlink(false);
                    self.move_hyperlink(src, dst);
                    dst.set_hyperlink(true);
                    dst_row.set_hyperlink(true);
                }
                if build_options::KITTY_GRAPHICS && src.codepoint() == graphics_unicode::PLACEHOLDER {
                    dst_row.set_kitty_virtual_placeholder(true);
                }
            }
        }

        // The destination row has styles if any of the cells are styled
        if !dst_row.styled() {
            // SAFETY: `len` cells within the row.
            let dst = unsafe { core::slice::from_raw_parts(dst_cells, len) };
            dst_row.set_styled(dst.iter().any(|c| c.style_id() != style::DEFAULT_ID));
        }

        // Clear our source row now that the copy is complete. We can NOT
        // use clearCells here because clearCells will garbage collect our
        // styles and graphames but we moved them above.
        //
        // Zero the cells as u64s since empirically this seems
        // to be a bit faster than using @memset(src_cells, .{})
        // SAFETY: `len` cells within the row.
        unsafe { core::ptr::write_bytes(src_cells, 0, len) };
        if len == self.size.cols as usize {
            src_row.set_grapheme(false);
            src_row.set_hyperlink(false);
            src_row.set_styled(false);
            if build_options::KITTY_GRAPHICS {
                src_row.set_kitty_virtual_placeholder(false);
            }
        }

        self.assert_integrity();
    }

    /// Swap two cells within the same row as quickly as possible.
    #[inline]
    pub fn swap_cells(&mut self, src: &mut Cell, dst: &mut Cell) {
        let mem = self.memory;

        // Graphemes are keyed by cell offset so we do have to move them.
        // We do this first so that all our grapheme state is correct.
        if src.has_grapheme() || dst.has_grapheme() {
            if src.has_grapheme() && !dst.has_grapheme() {
                self.move_grapheme(src, dst);
            } else if !src.has_grapheme() && dst.has_grapheme() {
                self.move_grapheme(dst, src);
            } else {
                // Both had graphemes, so we have to manually swap
                let src_offset = get_offset(mem, src as *const Cell);
                let dst_offset = get_offset(mem, dst as *const Cell);
                // SAFETY: `mem` is the page base.
                let map = unsafe { self.grapheme_map.map(mem) };
                let src_entry = map.get_entry(src_offset).unwrap();
                let dst_entry = map.get_entry(dst_offset).unwrap();
                // SAFETY: entry pointers are valid map slots.
                unsafe { core::ptr::swap(src_entry.value_ptr, dst_entry.value_ptr) };
            }
        }

        // Hyperlinks are keyed by cell offset.
        if src.hyperlink() || dst.hyperlink() {
            if src.hyperlink() && !dst.hyperlink() {
                self.move_hyperlink(src, dst);
            } else if !src.hyperlink() && dst.hyperlink() {
                self.move_hyperlink(dst, src);
            } else {
                // Both had hyperlinks, so we have to manually swap
                let src_offset = get_offset(mem, src as *const Cell);
                let dst_offset = get_offset(mem, dst as *const Cell);
                // SAFETY: `mem` is the page base.
                let map = unsafe { self.hyperlink_map.map(mem) };
                let src_entry = map.get_entry(src_offset).unwrap();
                let dst_entry = map.get_entry(dst_offset).unwrap();
                // SAFETY: entry pointers are valid map slots.
                unsafe { core::ptr::swap(src_entry.value_ptr, dst_entry.value_ptr) };
            }
        }

        // Copy the metadata. Note that we do NOT have to worry about
        // styles because styles are keyed by ID and we're preserving the
        // exact ref count and row state here.
        core::mem::swap(src, dst);

        self.assert_integrity();
    }

    /// Clear the cells in the given row. This will reclaim memory used
    /// by graphemes and styles. Note that if the style cleared is still
    /// active, Page cannot know this and it will still be ref counted down.
    /// The best solution for this is to artificially increment the ref count
    /// prior to calling this function.
    #[inline]
    pub fn clear_cells(&mut self, row: &mut Row, left: usize, end: usize) {
        let mem = self.memory;
        // Zig slices the row cells with a many-pointer, so `end` may exceed
        // `size.cols` (but never the row capacity) on mixed-width page lists.
        debug_assert!(left <= end && end <= self.capacity.cols as usize);
        // SAFETY: the range is within the row.
        let cells: *mut Cell = unsafe { row.cells().ptr(mem).add(left) };
        let len = end - left;
        let full = len == self.size.cols as usize;

        // If we have managed memory (styles, graphemes, or hyperlinks)
        // in this row then we go cell by cell and clear them if present.
        if row.grapheme() {
            for i in 0..len {
                // SAFETY: within the row.
                let cell = unsafe { &mut *cells.add(i) };
                if cell.has_grapheme() {
                    self.clear_grapheme(cell);
                }
            }

            // If we have no left/right scroll region we can be sure
            // that we've cleared all the graphemes, so we clear the
            // flag, otherwise we use the update function to update.
            if full {
                row.set_grapheme(false);
            } else {
                self.update_row_grapheme_flag(row);
            }
        }

        if row.hyperlink() {
            for i in 0..len {
                // SAFETY: within the row.
                let cell = unsafe { &mut *cells.add(i) };
                if cell.hyperlink() {
                    self.clear_hyperlink(cell);
                }
            }

            // If we have no left/right scroll region we can be sure
            // that we've cleared all the hyperlinks, so we clear the
            // flag, otherwise we use the update function to update.
            if full {
                row.set_hyperlink(false);
            } else {
                self.update_row_hyperlink_flag(row);
            }
        }

        if row.styled() {
            for i in 0..len {
                // SAFETY: within the row.
                let cell = unsafe { &*cells.add(i) };
                if cell.has_styling() {
                    // SAFETY: `mem` is the page base.
                    unsafe { self.styles.release(mem, cell.style_id()) };
                }
            }

            // If we have no left/right scroll region we can be sure
            // that we've cleared all the styles, so we clear the
            // flag, otherwise we use the update function to update.
            if full {
                row.set_styled(false);
            } else {
                self.update_row_styled_flag(row);
            }
        }

        if build_options::KITTY_GRAPHICS && full {
            row.set_kitty_virtual_placeholder(false);
        }

        // Zero the cells as u64s since empirically this seems
        // to be a bit faster than using @memset(cells, .{})
        // SAFETY: `len` cells within the row.
        unsafe { core::ptr::write_bytes(cells, 0, len) };

        self.assert_integrity();
    }

    /// Reset the given row to the default state: all cells zeroed and
    /// all row metadata reset, as if the row was never used. This
    /// reclaims memory used by graphemes, styles, etc. like clearCells.
    ///
    /// This must be used instead of clearCells whenever a row's storage
    /// is recycled. Clearing the cells alone is not enough because row
    /// metadata such as the wrap state and semantic prompt remain.
    #[inline]
    pub fn reset_row(&mut self, row: &mut Row) {
        let cols = self.size.cols as usize;
        self.clear_cells(row, 0, cols);
        row.reset();
    }

    /// Returns the hyperlink ID for the given cell.
    #[inline]
    pub fn lookup_hyperlink(&self, cell: &Cell) -> Option<hyperlink::Id> {
        let cell_offset = get_offset(self.memory, cell as *const Cell);
        // SAFETY: `self.memory` is the page base.
        let map = unsafe { self.hyperlink_map.map(self.memory) };
        map.get(cell_offset)
    }

    /// Clear the hyperlink from the given cell.
    ///
    /// In order to update the hyperlink flag on the row, call
    /// `updateRowHyperlinkFlag` after you finish clearing any
    /// hyperlinks in the row.
    pub fn clear_hyperlink(&mut self, cell: &mut Cell) {
        let mem = self.memory;

        // Get our ID
        let cell_offset = get_offset(mem, cell as *const Cell);
        // SAFETY: `mem` is the page base.
        let mut map = unsafe { self.hyperlink_map.map(mem) };
        let Some(entry) = map.get_entry(cell_offset) else {
            self.assert_integrity();
            return;
        };

        // Release our usage of this, free memory, unset flag
        // SAFETY: `mem` is the page base; the entry pointers are valid.
        unsafe { self.hyperlink_set.release(mem, *entry.value_ptr) };
        map.remove_by_ptr(entry.key_ptr);
        cell.set_hyperlink(false);

        self.assert_integrity();
    }

    /// Checks if the row contains any hyperlinks and sets
    /// the hyperlink flag to false if none are found.
    ///
    /// Call after removing hyperlinks in a row.
    #[inline]
    pub fn update_row_hyperlink_flag(&self, row: &mut Row) {
        // SAFETY: the row's cells are within the page.
        let cells = unsafe { core::slice::from_raw_parts(row.cells().ptr(self.memory), self.size.cols as usize) };
        if cells.iter().any(|c| c.hyperlink()) {
            return;
        }
        row.set_hyperlink(false);
    }

    /// Convert a hyperlink into a page entry, returning the ID.
    ///
    /// This does not de-dupe any strings, so if the URI, explicit ID,
    /// etc. is already in the strings table this will duplicate it.
    ///
    /// To release the memory associated with the given hyperlink,
    /// release the ID from the `hyperlink_set`. If the refcount reaches
    /// zero and the slot is needed then the context will reap the
    /// memory.
    pub fn insert_hyperlink(&mut self, link: &hyperlink::Hyperlink<'_>) -> Result<hyperlink::Id, InsertHyperlinkError> {
        let mem = self.memory;

        // Insert our URI into the page strings table.
        let page_uri: OffsetSlice<u8> = {
            // SAFETY: `mem` is the page base.
            let buf = unsafe { self.string_alloc.alloc::<u8>(mem, link.uri.len()) }
                .map_err(|_| InsertHyperlinkError::StringsOutOfMemory)?;
            // SAFETY: `buf` has `link.uri.len()` bytes.
            unsafe { core::ptr::copy_nonoverlapping(link.uri.as_ptr(), buf.cast::<u8>(), link.uri.len()) };
            OffsetSlice::new(get_offset(mem, buf.cast::<u8>().cast_const()), link.uri.len())
        };

        // Allocate an ID for our page memory if we have to.
        let page_id: hyperlink::PageEntryId = match &link.id {
            hyperlink::HyperlinkId::Explicit(id) => {
                // SAFETY: `mem` is the page base.
                let buf = match unsafe { self.string_alloc.alloc::<u8>(mem, id.len()) } {
                    Ok(v) => v,
                    Err(_) => {
                        // errdefer: free the URI.
                        // SAFETY: allocated above.
                        unsafe { self.string_alloc.free(mem, page_uri.slice_ptr(mem)) };
                        return Err(InsertHyperlinkError::StringsOutOfMemory);
                    }
                };
                // SAFETY: `buf` has `id.len()` bytes.
                unsafe { core::ptr::copy_nonoverlapping(id.as_ptr(), buf.cast::<u8>(), id.len()) };

                hyperlink::PageEntryId::Explicit(OffsetSlice::new(
                    get_offset(mem, buf.cast::<u8>().cast_const()),
                    id.len(),
                ))
            }

            hyperlink::HyperlinkId::Implicit(id) => hyperlink::PageEntryId::Implicit(*id),
        };

        // Build our entry
        let entry = hyperlink::PageEntry {
            id: page_id,
            uri: page_uri,
        };

        // Put our hyperlink into the hyperlink set to get an ID
        let ctx = hyperlink::SetContext::new(self as *mut Page);
        // SAFETY: all access goes through `ctx.page` (self).
        let added = unsafe { (*ctx.page).hyperlink_set.add_context((*ctx.page).memory, entry, &ctx) };
        match added {
            Ok(id) => Ok(id),
            Err(err) => {
                // errdefer: free the strings.
                // SAFETY: allocated above.
                unsafe {
                    if let hyperlink::PageEntryId::Explicit(slice) = page_id {
                        self.string_alloc.free(mem, slice.slice_ptr(mem));
                    }
                    self.string_alloc.free(mem, page_uri.slice_ptr(mem));
                }
                Err(match err {
                    AddError::OutOfMemory => InsertHyperlinkError::SetOutOfMemory,
                    AddError::NeedsRehash => InsertHyperlinkError::SetNeedsRehash,
                })
            }
        }
    }

    /// Set the hyperlink for the given cell. If the cell already has a
    /// hyperlink, then this will handle memory management and refcount
    /// update for the prior hyperlink.
    ///
    /// DOES NOT increment the reference count for the new hyperlink!
    ///
    /// Caller is responsible for updating the refcount in the hyperlink
    /// set as necessary by calling `use` if the id was not acquired with
    /// `add`.
    ///
    /// The only error returned is `HyperlinkMapOutOfMemory`.
    #[inline]
    pub fn set_hyperlink(&mut self, row: &mut Row, cell: &mut Cell, id: hyperlink::Id) -> Result<(), HyperlinkError> {
        let mem = self.memory;
        let cell_offset = get_offset(mem, cell as *const Cell);
        // SAFETY: `mem` is the page base.
        let mut map = unsafe { self.hyperlink_map.map(mem) };
        let gop = match map.get_or_put(cell_offset) {
            Ok(v) => v,
            Err(AllocError::OutOfMemory) => {
                // The hyperlink map capacity needs to be increased.
                self.assert_integrity();
                return Err(HyperlinkError::HyperlinkMapOutOfMemory);
            }
        };

        if gop.found_existing {
            // Always release the old hyperlink, because even if it's actually
            // the same as the one we're setting, we'd end up double-counting
            // if we left the reference count be, because the caller does not
            // know whether it's the same and will have increased the count
            // outside of this function.
            // SAFETY: valid entry pointer; `mem` is the page base.
            let old = unsafe { *gop.value_ptr };
            unsafe { self.hyperlink_set.release(mem, old) };

            // If the hyperlink matches then we don't need to do anything.
            if old == id {
                // It is possible for cell hyperlink to be false but row
                // must never be false. The cell hyperlink can be false because
                // in Terminal.print we clear the hyperlink for the cursor cell
                // before writing the cell again, so if someone prints over
                // a cell with a matching hyperlink this state can happen.
                // This is tested in Terminal.zig.
                assert!(row.hyperlink());
                cell.set_hyperlink(true);
                self.assert_integrity();
                return Ok(());
            }
        }

        // Set the hyperlink on the cell and in the map.
        // SAFETY: valid entry pointer.
        unsafe { *gop.value_ptr = id };
        cell.set_hyperlink(true);
        row.set_hyperlink(true);

        self.assert_integrity();
        Ok(())
    }

    /// Move the hyperlink from one cell to another. This can't fail
    /// because we avoid any allocations since we're just moving data.
    /// Destination must NOT have a hyperlink.
    #[inline]
    fn move_hyperlink(&mut self, src: &mut Cell, dst: &mut Cell) {
        assert!(src.hyperlink());
        assert!(!dst.hyperlink());

        let mem = self.memory;
        let src_offset = get_offset(mem, src as *const Cell);
        let dst_offset = get_offset(mem, dst as *const Cell);
        // SAFETY: `mem` is the page base.
        let mut map = unsafe { self.hyperlink_map.map(mem) };
        let entry = map.get_entry(src_offset).unwrap();
        // SAFETY: valid entry pointer.
        let value = unsafe { *entry.value_ptr };
        map.remove_by_ptr(entry.key_ptr);
        map.put_assume_capacity_no_clobber(dst_offset, value);

        // NOTE: We must not set src/dst.hyperlink here because this
        // function is used in various cases where we swap cell contents
        // and its unsafe. The flip side: the caller must be careful
        // to set the proper cell state to represent the move.
    }

    /// Returns the number of hyperlinks in the page. This isn't the byte
    /// size but the total number of unique cells that have hyperlink data.
    #[inline]
    pub fn hyperlink_count(&self) -> usize {
        // SAFETY: `self.memory` is the page base.
        unsafe { self.hyperlink_map.map(self.memory) }.count() as usize
    }

    /// Returns the hyperlink capacity for the page. This isn't the byte
    /// size but the number of unique cells that can have hyperlink data.
    #[inline]
    pub fn hyperlink_capacity(&self) -> usize {
        // SAFETY: `self.memory` is the page base.
        unsafe { self.hyperlink_map.map(self.memory) }.max_load() as usize
    }

    /// Set the graphemes for the given cell. This asserts that the cell
    /// has no graphemes set, and only contains a single codepoint. Input
    /// beyond grapheme_max_len is ignored.
    #[inline]
    pub fn set_graphemes(&mut self, row: &mut Row, cell: &mut Cell, cps: &[u32]) -> Result<(), GraphemeError> {
        assert!(cell.codepoint() > 0);
        assert!(cell.content_tag() == ContentTag::Codepoint);

        let mem = self.memory;
        let cell_offset = get_offset(mem, cell as *const Cell);
        // SAFETY: `mem` is the page base.
        let mut map = unsafe { self.grapheme_map.map(mem) };
        let stored_cps = &cps[..cps.len().min(GRAPHEME_MAX_LEN)];

        // SAFETY: `mem` is the page base.
        let slice = match unsafe { self.grapheme_alloc.alloc::<u32>(mem, stored_cps.len()) } {
            Ok(v) => v,
            Err(AllocError::OutOfMemory) => {
                // The grapheme alloc capacity needs to be increased.
                self.assert_integrity();
                return Err(GraphemeError::GraphemeAllocOutOfMemory);
            }
        };
        // SAFETY: `slice` has room for `stored_cps.len()` codepoints and
        // doesn't overlap `cps` (fresh allocation).
        unsafe { core::ptr::copy_nonoverlapping(stored_cps.as_ptr(), slice.cast::<u32>(), stored_cps.len()) };

        if map
            .put_no_clobber(
                cell_offset,
                OffsetSlice::new(get_offset(mem, slice.cast::<u32>().cast_const()), slice.len()),
            )
            .is_err()
        {
            // errdefer: free the slice.
            // SAFETY: allocated above.
            unsafe { self.grapheme_alloc.free(mem, slice) };
            // The grapheme map capacity needs to be increased.
            self.assert_integrity();
            return Err(GraphemeError::GraphemeMapOutOfMemory);
        }

        cell.set_content_tag(ContentTag::CodepointGrapheme);
        row.set_grapheme(true);

        self.assert_integrity();
        Ok(())
    }

    /// Append a codepoint to the given cell as a grapheme. Once the cell has
    /// grapheme_max_len suffix codepoints, additional codepoints are ignored.
    pub fn append_grapheme(&mut self, row: &mut Row, cell: &mut Cell, cp: u32) -> Result<(), AllocError> {
        let result = self.append_grapheme_inner(row, cell, cp);
        self.assert_integrity();
        result
    }

    fn append_grapheme_inner(&mut self, row: &mut Row, cell: &mut Cell, cp: u32) -> Result<(), AllocError> {
        if build_options::SLOW_RUNTIME_SAFETY {
            assert!(cell.codepoint() != 0);
        }

        let mem = self.memory;
        let cell_offset = get_offset(mem, cell as *const Cell);
        // SAFETY: `mem` is the page base.
        let mut map = unsafe { self.grapheme_map.map(mem) };

        // If this cell has no graphemes, we can go faster by knowing we
        // need to allocate a new grapheme slice and update the map.
        if cell.content_tag() != ContentTag::CodepointGrapheme {
            // SAFETY: `mem` is the page base.
            let cps = unsafe { self.grapheme_alloc.alloc::<u32>(mem, 1)? };
            // SAFETY: one codepoint was allocated.
            unsafe { *cps.cast::<u32>() = cp };

            if let Err(e) = map.put_no_clobber(
                cell_offset,
                OffsetSlice::new(get_offset(mem, cps.cast::<u32>().cast_const()), 1),
            ) {
                // SAFETY: allocated above.
                unsafe { self.grapheme_alloc.free(mem, cps) };
                return Err(e);
            }

            cell.set_content_tag(ContentTag::CodepointGrapheme);
            row.set_grapheme(true);

            return Ok(());
        }

        // The cell already has graphemes. We need to append to the existing
        // grapheme slice and update the map.
        assert!(row.grapheme());

        let slice_ptr = map.get_ptr(cell_offset).unwrap();
        // SAFETY: valid map value pointer.
        let slice = unsafe { &mut *slice_ptr };

        // Terminal input is untrusted. In addition to bounding memory, this
        // prevents repeated chunk growth and copying from becoming quadratic.
        if slice.len >= GRAPHEME_MAX_LEN {
            return Ok(());
        }

        // If our slice len doesn't divide evenly by the grapheme chunk
        // length then we can utilize the additional chunk space.
        if slice.len % GRAPHEME_CHUNK_LEN != 0 {
            let cps = slice.offset.ptr(mem);
            // SAFETY: the chunk has room past `len`.
            unsafe { *cps.add(slice.len) = cp };
            slice.len += 1;
            return Ok(());
        }

        // We are out of chunk space. There is no fast path here. We need
        // to allocate a larger chunk. This is a very slow path. We expect
        // most graphemes to fit within our chunk size.
        // SAFETY: `mem` is the page base.
        let cps = unsafe { self.grapheme_alloc.alloc::<u32>(mem, slice.len + 1)? };
        let old_cps = slice.slice_ptr(mem);
        // SAFETY: both are valid, non-overlapping allocations.
        unsafe {
            core::ptr::copy_nonoverlapping(old_cps.cast::<u32>(), cps.cast::<u32>(), slice.len);
            *cps.cast::<u32>().add(slice.len) = cp;
        }
        *slice = OffsetSlice::new(get_offset(mem, cps.cast::<u32>().cast_const()), slice.len + 1);

        // Free our old chunk
        // SAFETY: the old chunk was allocated from this allocator.
        unsafe { self.grapheme_alloc.free(mem, old_cps) };

        Ok(())
    }

    /// Returns the codepoints for the given cell. These are the codepoints
    /// in addition to the first codepoint. The first codepoint is NOT
    /// included since it is on the cell itself.
    #[inline]
    pub fn lookup_grapheme(&self, cell: &Cell) -> Option<&[u32]> {
        let cell_offset = get_offset(self.memory, cell as *const Cell);
        // SAFETY: `self.memory` is the page base.
        let map = unsafe { self.grapheme_map.map(self.memory) };
        let slice = map.get(cell_offset)?;
        // SAFETY: the slice lives in the page memory.
        Some(unsafe { slice.slice(self.memory) })
    }

    /// Move the graphemes from one cell to another. This can't fail
    /// because we avoid any allocations since we're just moving data.
    ///
    /// WARNING: This will NOT change the content_tag on the cells because
    /// there are scenarios where we want to move graphemes without changing
    /// the content tag. Callers beware but assertIntegrity should catch this.
    #[inline]
    pub fn move_grapheme(&mut self, src: &mut Cell, dst: &mut Cell) {
        if build_options::SLOW_RUNTIME_SAFETY {
            assert!(src.has_grapheme());
            assert!(!dst.has_grapheme());
        }

        let mem = self.memory;
        let src_offset = get_offset(mem, src as *const Cell);
        let dst_offset = get_offset(mem, dst as *const Cell);
        // SAFETY: `mem` is the page base.
        let mut map = unsafe { self.grapheme_map.map(mem) };
        let entry = map.get_entry(src_offset).unwrap();
        // SAFETY: valid entry pointer.
        let value = unsafe { *entry.value_ptr };
        map.remove_by_ptr(entry.key_ptr);
        map.put_assume_capacity_no_clobber(dst_offset, value);
    }

    /// Clear the graphemes for a given cell.
    ///
    /// In order to update the grapheme flag on the row, call
    /// `updateRowGraphemeFlag` after you finish clearing any
    /// graphemes in the row.
    pub fn clear_grapheme(&mut self, cell: &mut Cell) {
        if build_options::SLOW_RUNTIME_SAFETY {
            assert!(cell.has_grapheme());
        }

        // Get our entry in the map, which must exist
        let mem = self.memory;
        let cell_offset = get_offset(mem, cell as *const Cell);
        // SAFETY: `mem` is the page base.
        let mut map = unsafe { self.grapheme_map.map(mem) };
        let entry = map.get_entry(cell_offset).unwrap();

        // Free our grapheme data
        // SAFETY: valid entry pointer; the data was allocated from this
        // allocator.
        unsafe {
            let cps = (*entry.value_ptr).slice_ptr(mem);
            self.grapheme_alloc.free(mem, cps);
        }

        // Remove the entry
        map.remove_by_ptr(entry.key_ptr);

        // Mark that we no longer have graphemes by changing the content tag.
        cell.set_content_tag(ContentTag::Codepoint);

        self.assert_integrity();
    }

    /// Checks if the row contains any graphemes and sets
    /// the grapheme flag to false if none are found.
    ///
    /// Call after removing graphemes in a row.
    #[inline]
    pub fn update_row_grapheme_flag(&self, row: &mut Row) {
        // SAFETY: the row's cells are within the page.
        let cells = unsafe { core::slice::from_raw_parts(row.cells().ptr(self.memory), self.size.cols as usize) };
        if cells.iter().any(|c| c.has_grapheme()) {
            return;
        }
        row.set_grapheme(false);
    }

    /// Returns the number of graphemes in the page. This isn't the byte
    /// size but the total number of unique cells that have grapheme data.
    #[inline]
    pub fn grapheme_count(&self) -> usize {
        // SAFETY: `self.memory` is the page base.
        unsafe { self.grapheme_map.map(self.memory) }.count() as usize
    }

    /// Returns the grapheme capacity for the page. This isn't the byte
    /// size but the number of unique cells that can have grapheme data.
    #[inline]
    pub fn grapheme_capacity(&self) -> usize {
        // SAFETY: `self.memory` is the page base.
        unsafe { self.grapheme_map.map(self.memory) }.capacity() as usize
    }

    /// Checks if the row contains any styles and sets
    /// the styled flag to false if none are found.
    ///
    /// Call after removing styles in a row.
    #[inline]
    pub fn update_row_styled_flag(&self, row: &mut Row) {
        // SAFETY: the row's cells are within the page.
        let cells = unsafe { core::slice::from_raw_parts(row.cells().ptr(self.memory), self.size.cols as usize) };
        if cells.iter().any(|c| c.has_styling()) {
            return;
        }
        row.set_styled(false);
    }

    /// Returns true if this page is dirty at all.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        if self.dirty {
            return true;
        }
        // SAFETY: rows are within the page memory.
        let rows = unsafe { core::slice::from_raw_parts(self.rows.ptr(self.memory), self.size.rows as usize) };
        rows.iter().any(|row| row.dirty())
    }

    /// The memory layout for a page given a desired minimum cols
    /// and rows size.
    ///
    /// The backing memory is laid out as the row headers, the cell
    /// array, and then the metadata block (see `MetaLayout`):
    ///
    ///   [rows][cells][styles, graphemes, strings, hyperlinks]
    ///
    /// Row headers always start at offset zero. The cell array is aligned
    /// to a cache line (see `cells_align`). Initializing a page writes only
    /// the row headers: the cells and every metadata member treat zero as
    /// their empty state, so the OS pages behind everything past the row
    /// headers stay untouched until something is stored in them.
    #[inline]
    pub fn layout(cap: Capacity) -> Layout {
        let meta = MetaLayout::init(cap);

        let rows_count: usize = cap.rows as usize;
        let rows_start = 0;
        let rows_end: usize = rows_start + (rows_count * core::mem::size_of::<Row>());

        let cells_count: usize = cap.cols as usize * cap.rows as usize;
        let cells_start = align_forward(rows_end, CELLS_ALIGN);
        let cells_end = cells_start + (cells_count * core::mem::size_of::<Cell>());

        let meta_start = align_forward(cells_end, MetaLayout::ALIGNMENT);
        let meta_end = meta_start + meta.total_size;

        let total_size = align_forward(meta_end, PAGE_SIZE_MIN);

        Layout {
            total_size,
            rows_start,
            rows_size: rows_end - rows_start,
            cells_start,
            cells_size: cells_end - cells_start,
            styles_start: meta_start + meta.styles_start,
            styles_layout: meta.styles_layout,
            grapheme_alloc_start: meta_start + meta.grapheme_alloc_start,
            grapheme_alloc_layout: meta.grapheme_alloc_layout,
            grapheme_map_start: meta_start + meta.grapheme_map_start,
            grapheme_map_layout: meta.grapheme_map_layout,
            string_alloc_start: meta_start + meta.string_alloc_start,
            string_alloc_layout: meta.string_alloc_layout,
            hyperlink_map_start: meta_start + meta.hyperlink_map_start,
            hyperlink_map_layout: meta.hyperlink_map_layout,
            hyperlink_set_start: meta_start + meta.hyperlink_set_start,
            hyperlink_set_layout: meta.hyperlink_set_layout,
            capacity: cap,
        }
    }
}

/// (Zig: `Page.Layout`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub total_size: usize,
    pub rows_start: usize,
    pub rows_size: usize,
    pub cells_start: usize,
    pub cells_size: usize,
    pub styles_start: usize,
    pub styles_layout: crate::terminal::ref_counted_set::Layout<StyleId>,
    pub grapheme_alloc_start: usize,
    pub grapheme_alloc_layout: crate::terminal::bitmap_allocator::Layout,
    pub grapheme_map_start: usize,
    pub grapheme_map_layout: hash_map::Layout,
    pub string_alloc_start: usize,
    pub string_alloc_layout: crate::terminal::bitmap_allocator::Layout,
    pub hyperlink_map_start: usize,
    pub hyperlink_map_layout: hash_map::Layout,
    pub hyperlink_set_start: usize,
    pub hyperlink_set_layout: crate::terminal::ref_counted_set::Layout<hyperlink::Id>,
    pub capacity: Capacity,
}

/// Meta is everything that isn't the grid, such as styles, graphemes,
/// etc. `layout` places it directly after the grid. (Zig: `Page.MetaLayout`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaLayout {
    /// The size of the block including internal alignment padding.
    pub total_size: usize,
    pub styles_start: usize,
    pub styles_layout: crate::terminal::ref_counted_set::Layout<StyleId>,
    pub grapheme_alloc_start: usize,
    pub grapheme_alloc_layout: crate::terminal::bitmap_allocator::Layout,
    pub grapheme_map_start: usize,
    pub grapheme_map_layout: hash_map::Layout,
    pub string_alloc_start: usize,
    pub string_alloc_layout: crate::terminal::bitmap_allocator::Layout,
    pub hyperlink_set_start: usize,
    pub hyperlink_set_layout: crate::terminal::ref_counted_set::Layout<hyperlink::Id>,
    pub hyperlink_map_start: usize,
    pub hyperlink_map_layout: hash_map::Layout,
}

impl MetaLayout {
    /// The alignment of the block's start: the largest alignment any
    /// member requires, so that the member offsets above don't depend
    /// on where the block is placed.
    pub const ALIGNMENT: usize = max_usize(
        max_usize(
            max_usize(StyleSet::BASE_ALIGN, GraphemeAlloc::BASE_ALIGN),
            max_usize(GraphemeMap::BASE_ALIGN, StringAlloc::BASE_ALIGN),
        ),
        max_usize(hyperlink::Set::BASE_ALIGN, hyperlink::Map::BASE_ALIGN),
    );

    /// Compute the layout of the block for the given capacity.
    pub fn init(cap: Capacity) -> MetaLayout {
        let styles_layout = StyleSet::layout(cap.styles as usize);
        let styles_start = 0;
        let styles_end = styles_start + styles_layout.total_size;

        let grapheme_alloc_layout = GraphemeAlloc::layout(cap.grapheme_bytes as usize);
        let grapheme_alloc_start = align_forward(styles_end, GraphemeAlloc::BASE_ALIGN);
        let grapheme_alloc_end = grapheme_alloc_start + grapheme_alloc_layout.total_size;

        let grapheme_count: usize = if cap.grapheme_bytes == 0 {
            0
        } else {
            // Use divCeil to match GraphemeAlloc.layout() which uses alignForward,
            // ensuring grapheme_map has capacity when grapheme_alloc has chunks.
            let base = (cap.grapheme_bytes as usize).div_ceil(GRAPHEME_CHUNK);
            base.next_power_of_two()
        };
        let grapheme_map_layout = GraphemeMap::layout(grapheme_count as hash_map::Size);
        let grapheme_map_start = align_forward(grapheme_alloc_end, GraphemeMap::BASE_ALIGN);
        let grapheme_map_end = grapheme_map_start + grapheme_map_layout.total_size;

        let string_layout = StringAlloc::layout(cap.string_bytes as usize);
        let string_start = align_forward(grapheme_map_end, StringAlloc::BASE_ALIGN);
        let string_end = string_start + string_layout.total_size;

        let hyperlink_count = cap.hyperlink_bytes as usize / core::mem::size_of::<hyperlink::SetItem>();
        let hyperlink_set_layout = hyperlink::Set::layout(hyperlink_count);
        let hyperlink_set_start = align_forward(string_end, hyperlink::Set::BASE_ALIGN);
        let hyperlink_set_end = hyperlink_set_start + hyperlink_set_layout.total_size;

        let hyperlink_map_count: u32 = if hyperlink_count == 0 {
            0
        } else {
            u32::try_from(hyperlink_count * HYPERLINK_CELL_MULTIPLIER).unwrap_or(u32::MAX)
        };
        let hyperlink_map_layout = hyperlink::Map::layout(hyperlink_map_count);
        let hyperlink_map_start = align_forward(hyperlink_set_end, hyperlink::Map::BASE_ALIGN);
        let hyperlink_map_end = hyperlink_map_start + hyperlink_map_layout.total_size;

        MetaLayout {
            total_size: hyperlink_map_end,
            styles_start,
            styles_layout,
            grapheme_alloc_start,
            grapheme_alloc_layout,
            grapheme_map_start,
            grapheme_map_layout,
            string_alloc_start: string_start,
            string_alloc_layout: string_layout,
            hyperlink_set_start,
            hyperlink_set_layout,
            hyperlink_map_start,
            hyperlink_map_layout,
        }
    }
}

/// The standard capacity for a page that doesn't have special
/// requirements. This is enough to support a very large number of cells.
/// The standard capacity is chosen as the fast-path for allocation since
/// pages of standard capacity use a pooled allocator instead of single-use
/// mmaps.
pub const STD_CAPACITY: Capacity = Capacity {
    cols: 215,
    rows: 215,
    styles: 128,
    grapheme_bytes: if cfg!(test) { 512 } else { 8192 },
    ..Capacity::new(0, 0)
};

/// The size of this page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    pub cols: size::CellCountInt,
    pub rows: size::CellCountInt,
}

/// Capacity of this page.
///
/// This capacity can be maxed out (every field max) and still fit
/// within a 64-bit memory space. If you need more than this, you will
/// need to split data across separate pages.
///
/// For 32-bit systems, it is possible to overflow the addressable
/// space and this is something we still need to address in the future
/// likely by limiting the maximum capacity on 32-bit systems further.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    /// Number of columns and rows we can know about.
    pub cols: size::CellCountInt,
    pub rows: size::CellCountInt,

    /// Number of unique styles that can be used on this page.
    pub styles: size::StyleCountInt,

    /// Number of bytes to allocate for hyperlink data. Note that the
    /// amount of data used for hyperlinks in total is more than this because
    /// hyperlinks use string data as well as a small amount of lookup metadata.
    /// This number is a rough approximation.
    pub hyperlink_bytes: size::HyperlinkCountInt,

    /// Number of bytes to allocate for grapheme data.
    pub grapheme_bytes: size::GraphemeBytesInt,

    /// Number of bytes to allocate for strings.
    pub string_bytes: size::StringBytesInt,
}

impl Default for Capacity {
    /// All defaults with zero cols/rows (Zig has no defaults for those).
    fn default() -> Self {
        Capacity::new(0, 0)
    }
}

/// The changes to make in `adjust`. Fields left as null are unchanged.
/// (Zig: `Capacity.Adjustment`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Adjustment {
    /// The new number of columns. The number of rows changes to fit.
    pub cols: Option<size::CellCountInt>,
}

impl Capacity {
    /// A capacity with the given cols/rows and every other field at its
    /// default (Zig: `.{ .cols = cols, .rows = rows }`).
    pub const fn new(cols: size::CellCountInt, rows: size::CellCountInt) -> Capacity {
        Capacity {
            cols,
            rows,
            styles: 16,
            hyperlink_bytes: HYPERLINK_BYTES_DEFAULT as size::HyperlinkCountInt,
            grapheme_bytes: GRAPHEME_BYTES_DEFAULT as size::GraphemeBytesInt,
            string_bytes: STRING_BYTES_DEFAULT as size::StringBytesInt,
        }
    }

    /// Returns the maximum number of columns that can be used with this
    /// capacity while still fitting at least one row. Returns null if even
    /// a single column cannot fit (which would indicate an unusable capacity).
    ///
    /// Note that this is the maximum number of columns that never increases
    /// the amount of memory the original capacity will take. If you modify
    /// the original capacity to add rows, then you can fit more columns.
    pub fn max_cols(&self) -> Option<size::CellCountInt> {
        let available = self.available_bytes_for_grid();

        // A single row's header occupies a whole cell-aligned region
        // ahead of the cells. If we can't even fit that, return null.
        let row_region = align_forward(core::mem::size_of::<Row>(), CELLS_ALIGN);
        if available <= row_region {
            return None;
        }

        // We do the math of how many columns we can fit in the remaining
        // bytes ignoring the metadata of a row.
        let max_cols = (available - row_region) / core::mem::size_of::<Cell>();

        // Clamp to CellCountInt max
        Some(max_cols.min(size::CellCountInt::MAX as usize) as size::CellCountInt)
    }

    /// Returns a copy of this capacity with the requested changes applied,
    /// without making the page use any more memory.
    ///
    /// Changing the number of columns changes how much memory each row
    /// needs. To stay within the original memory size, `adjust` changes the
    /// number of rows to match. Wider pages fit fewer rows and narrower pages
    /// fit more. All other fields, such as `styles`, are left unchanged.
    ///
    /// The result never uses more memory than the original, but it can use
    /// less. This happens for two reasons:
    ///
    ///   - Only whole rows are counted. Leftover memory that is too small
    ///     for one more row goes unused.
    ///   - The number of rows is capped at `maxInt(size.CellCountInt)`.
    ///     Narrowing a very wide page can free enough memory for more rows
    ///     than a page can hold. The extra memory goes unused.
    ///
    /// Returns `error.OutOfMemory` if not even one row of the requested
    /// width fits. Use `maxCols` to find the widest column count that fits.
    pub fn adjust(&self, req: Adjustment) -> Result<Capacity, AllocError> {
        let mut adjusted = *self;
        if let Some(cols) = req.cols {
            let total_size = Page::layout(*self).total_size;
            let available = self.available_bytes_for_grid();

            // The size per row is:
            //   - The row metadata itself
            //   - The cells per row (n=cols)
            let bytes_per_row: usize = core::mem::size_of::<Row>() + core::mem::size_of::<Cell>() * cols as usize;

            // A page can't hold more than maxInt(CellCountInt) rows, so cap
            // the count here. Using fewer rows than would fit is always safe
            // because it only makes the page smaller.
            let mut new_rows: usize = (available / bytes_per_row).min(size::CellCountInt::MAX as usize);

            // The cell array is aligned to a cache line, so the padding
            // between the row headers and the cells depends on the row
            // count. Trim rows until the layout fits the original size.
            // The padding is less than a cache line so this takes a
            // handful of iterations at most.
            adjusted.cols = cols;
            while new_rows > 0 {
                adjusted.rows = new_rows as size::CellCountInt;
                if Page::layout(adjusted).total_size <= total_size {
                    break;
                }
                new_rows -= 1;
            }

            // If our rows go to zero then we can't fit any row metadata
            // for the desired number of columns.
            if new_rows == 0 {
                return Err(AllocError::OutOfMemory);
            }
        }

        Ok(adjusted)
    }

    /// Computes the number of bytes available for the row headers and
    /// cells in the page: the page size minus the metadata block.
    fn available_bytes_for_grid(&self) -> usize {
        const _: () = assert!(CELLS_ALIGN % MetaLayout::ALIGNMENT == 0);
        const _: () = assert!(core::mem::size_of::<Cell>() % MetaLayout::ALIGNMENT == 0);

        let l = Page::layout(*self);
        l.total_size - MetaLayout::init(*self).total_size
    }
}

/// A bit range within a packed `u64` (offset and width), used to describe
/// the fields of [`Row`] and [`Cell`] for [`field_mask`] and [`Mask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitField {
    pub offset: u32,
    pub bits: u32,
}

impl BitField {
    pub const fn new(offset: u32, bits: u32) -> BitField {
        BitField { offset, bits }
    }

    /// The mask covering this field.
    #[inline(always)]
    pub const fn mask(self) -> u64 {
        let ones: u64 = if self.bits >= 64 { u64::MAX } else { (1u64 << self.bits) - 1 };
        ones << self.offset
    }

    #[inline(always)]
    const fn get(self, v: u64) -> u64 {
        (v & self.mask()) >> self.offset
    }

    #[inline(always)]
    const fn set(self, v: u64, value: u64) -> u64 {
        (v & !self.mask()) | ((value << self.offset) & self.mask())
    }
}

/// The bit layout of [`Row`] (Zig: the fields of `Row`, a `packed
/// struct(u64)`, LSB first).
pub mod row_fields {
    use super::BitField;

    /// `cells: Offset(Cell)` (a `packed struct(u32)`).
    pub const CELLS: BitField = BitField::new(0, 32);
    pub const WRAP: BitField = BitField::new(32, 1);
    pub const WRAP_CONTINUATION: BitField = BitField::new(33, 1);
    pub const GRAPHEME: BitField = BitField::new(34, 1);
    pub const STYLED: BitField = BitField::new(35, 1);
    pub const HYPERLINK: BitField = BitField::new(36, 1);
    pub const SEMANTIC_PROMPT: BitField = BitField::new(37, 2);
    pub const KITTY_VIRTUAL_PLACEHOLDER: BitField = BitField::new(39, 1);
    pub const DIRTY: BitField = BitField::new(40, 1);
    pub const PADDING: BitField = BitField::new(41, 23);
}

/// The bit layout of [`Cell`] (Zig: the fields of `Cell`, a `packed
/// struct(u64)`, LSB first). Nested field paths such as
/// `content.codepoint.data` are flattened into `CONTENT_CODEPOINT_DATA`.
pub mod cell_fields {
    use super::BitField;

    pub const CONTENT_TAG: BitField = BitField::new(0, 2);
    /// The whole `content` packed union (24 bits).
    pub const CONTENT: BitField = BitField::new(2, 24);
    /// `content.codepoint.data` (u21).
    pub const CONTENT_CODEPOINT_DATA: BitField = BitField::new(2, 21);
    /// `content.color_palette.data` (u8).
    pub const CONTENT_COLOR_PALETTE_DATA: BitField = BitField::new(2, 8);
    /// `content.color_rgb` (r, g, b; 24 bits).
    pub const CONTENT_COLOR_RGB: BitField = BitField::new(2, 24);
    pub const STYLE_ID: BitField = BitField::new(26, 16);
    pub const WIDE: BitField = BitField::new(42, 2);
    pub const PROTECTED: BitField = BitField::new(44, 1);
    pub const HYPERLINK: BitField = BitField::new(45, 1);
    pub const SEMANTIC_CONTENT: BitField = BitField::new(46, 2);
    pub const PADDING: BitField = BitField::new(48, 16);
}

/// The semantic prompt state of the row. See `semantic_prompt`.
/// (Zig: `Row.SemanticPrompt`, `enum(u2)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum SemanticPrompt {
    /// No prompt cells in this row.
    #[default]
    None = 0,
    /// Prompt cells exist in this row and this is a primary prompt
    /// line. A primary prompt line is one that is not a continuation
    /// and is the beginning of a prompt.
    Prompt = 1,
    /// Prompt cells exist in this row that had k=c set (continuation)
    /// line. This is used as a way to detect when a line should
    /// be considered part of some prior prompt. If no prior prompt
    /// is found, the last (most historical) prompt continuation line is
    /// considered the prompt.
    PromptContinuation = 2,
}

impl SemanticPrompt {
    #[inline]
    fn from_bits(v: u64) -> SemanticPrompt {
        match v {
            0 => SemanticPrompt::None,
            1 => SemanticPrompt::Prompt,
            2 => SemanticPrompt::PromptContinuation,
            // Not a valid value of the enum; Zig would treat this as
            // illegal behavior.
            _ => unreachable!("invalid SemanticPrompt bits"),
        }
    }
}

/// Packed types usable with [`Mask`] (Zig: packed structs with a `u64`
/// backing integer).
pub trait Packed: Copy {
    fn to_bits(self) -> u64;
}

/// A row in a page (Zig: `Row`, a `packed struct(u64)`).
///
/// See [`row_fields`] for the exact bit layout.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Row(u64);

/// The backing integer of [`Row`] (Zig: `Row.Backing`).
pub type RowBacking = u64;

/// C ABI type of [`Row`] (Zig: `Row.C`).
pub type RowC = u64;

macro_rules! bool_field {
    ($fields:ident, $get:ident, $set:ident, $field:ident) => {
        #[inline(always)]
        pub fn $get(&self) -> bool {
            $fields::$field.get(self.0) != 0
        }

        #[inline(always)]
        pub fn $set(&mut self, v: bool) {
            self.0 = $fields::$field.set(self.0, v as u64);
        }
    };
}

impl Row {
    /// A row with the given cells offset and every other field at its
    /// default (Zig: `.{ .cells = cells }`).
    #[inline]
    pub const fn init(cells: Offset<Cell>) -> Row {
        Row(cells.offset as u64)
    }

    #[inline(always)]
    pub const fn from_bits(bits: u64) -> Row {
        Row(bits)
    }

    #[inline(always)]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// Returns this row as a C ABI value.
    #[inline]
    pub fn cval(self) -> RowC {
        self.0
    }

    /// The cells in the row offset from the page.
    #[inline(always)]
    pub fn cells(&self) -> Offset<Cell> {
        Offset::new(row_fields::CELLS.get(self.0) as u32)
    }

    #[inline(always)]
    pub fn set_cells(&mut self, cells: Offset<Cell>) {
        self.0 = row_fields::CELLS.set(self.0, cells.offset as u64);
    }

    // True if this row is soft-wrapped. The first cell of the next
    // row is a continuation of this row.
    bool_field!(row_fields, wrap, set_wrap, WRAP);

    // True if the previous row to this one is soft-wrapped and
    // this row is a continuation of that row.
    bool_field!(row_fields, wrap_continuation, set_wrap_continuation, WRAP_CONTINUATION);

    // True if any of the cells in this row have multi-codepoint
    // grapheme clusters. If this is true, some fast paths are not
    // possible because erasing for example may need to clear existing
    // grapheme data.
    bool_field!(row_fields, grapheme, set_grapheme, GRAPHEME);

    // True if any of the cells in this row have a ref-counted style.
    // This can have false positives but never a false negative. Meaning:
    // this will be set to true the first time a style is used, but it
    // will not be set to false if the style is no longer used, because
    // checking for that condition is too expensive.
    //
    // Why have this weird false positive flag at all? This makes VT operations
    // that erase cells (such as insert lines, delete lines, erase chars,
    // etc.) MUCH MUCH faster in the case that the row was never styled.
    // At the time of writing this, the speed difference is around 4x.
    bool_field!(row_fields, styled, set_styled, STYLED);

    // True if any of the cells in this row are part of a hyperlink.
    // This is similar to styled: it can have false positives but never
    // false negatives. This is used to optimize hyperlink operations.
    bool_field!(row_fields, hyperlink, set_hyperlink, HYPERLINK);

    /// The semantic prompt state for this row.
    ///
    /// This is ONLY meant to note if there are ANY cells in this
    /// row that are part of a prompt. This is an optimization for more
    /// efficiently implementing jump-to-prompt operations.
    ///
    /// This may contain false positives but never false negatives. If
    /// this is set, you should still check individual cells to see if they
    /// have prompt semantics.
    #[inline(always)]
    pub fn semantic_prompt(&self) -> SemanticPrompt {
        SemanticPrompt::from_bits(row_fields::SEMANTIC_PROMPT.get(self.0))
    }

    #[inline(always)]
    pub fn set_semantic_prompt(&mut self, v: SemanticPrompt) {
        self.0 = row_fields::SEMANTIC_PROMPT.set(self.0, v as u64);
    }

    // True if this row contains a virtual placeholder for the Kitty
    // graphics protocol. (U+10EEEE)
    // Note: We keep this as memory-using even if the kitty graphics
    // feature is disabled because we want to keep our padding and
    // everything throughout the same.
    bool_field!(
        row_fields,
        kitty_virtual_placeholder,
        set_kitty_virtual_placeholder,
        KITTY_VIRTUAL_PLACEHOLDER
    );

    // True if this row is dirty and requires a redraw. This is set to true
    // by any operation that modifies the row's contents or position, and
    // consumers of the page are expected to clear it when they redraw.
    //
    // Dirty status is only ever meant to convey that one or more cells in
    // the row have changed visually. A cell which changes in a way that
    // doesn't affect the visual representation may not be marked as dirty.
    //
    // Dirty tracking may have false positives but should never have false
    // negatives. A false negative would result in a visual artifact on the
    // screen.
    bool_field!(row_fields, dirty, set_dirty, DIRTY);

    /// Returns true if this row has any managed memory outside of the
    /// row structure (graphemes, styles, etc.)
    #[inline]
    pub fn managed_memory(&self) -> bool {
        // Ordered on purpose for likelihood.
        self.styled() || self.hyperlink() || self.grapheme()
    }

    /// Reset all row metadata to the default state, preserving only
    /// the cells offset, and mark the row dirty. This is a single
    /// 8-byte store.
    ///
    /// This must be applied to any row whose storage is recycled as a
    /// blank row or retired into unused page capacity, in addition to
    /// clearing its cells (in either order; this doesn't touch cell
    /// memory). See Page.resetRow, which does both, for details. This
    /// exists separately for callers that clear the cells in a
    /// specialized way (e.g. filling with a background-colored blank
    /// cell rather than zeroing).
    ///
    /// Asserts that the row has no managed memory: releasing that is
    /// the cell-clearing side's job and must happen while the flags
    /// are still accurate.
    #[inline]
    pub fn reset(&mut self) {
        assert!(!self.managed_memory());
        let mut row = Row::init(self.cells());
        row.set_dirty(true);
        *self = row;
    }
}

impl Packed for Row {
    #[inline(always)]
    fn to_bits(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Row")
            .field("cells", &self.cells().offset)
            .field("wrap", &self.wrap())
            .field("wrap_continuation", &self.wrap_continuation())
            .field("grapheme", &self.grapheme())
            .field("styled", &self.styled())
            .field("hyperlink", &self.hyperlink())
            .field("semantic_prompt", &self.semantic_prompt())
            .field("kitty_virtual_placeholder", &self.kitty_virtual_placeholder())
            .field("dirty", &self.dirty())
            .finish()
    }
}

/// The content tag dictates the active tag in content and possibly
/// some other behaviors. (Zig: `Cell.ContentTag`, `enum(u2)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum ContentTag {
    /// A single codepoint, could be zero to be empty cell.
    #[default]
    Codepoint = 0,

    /// A codepoint that is part of a multi-codepoint grapheme cluster.
    /// The codepoint tag is active in content, but also expect more
    /// codepoints in the grapheme data.
    CodepointGrapheme = 1,

    /// The cell has no text but only a background color. This is an
    /// optimization so that cells with only backgrounds don't take up
    /// style map space and also don't require a style map lookup.
    BgColorPalette = 2,
    BgColorRgb = 3,
}

/// (Zig: `Cell.RGB`, a `packed struct` of r, g, b; r in the low bits.)
pub type Rgb = color::Rgb;

/// (Zig: `Cell.Wide`, `enum(u2)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Wide {
    /// Not a wide character, cell width 1.
    #[default]
    Narrow = 0,

    /// Wide character, cell width 2.
    Wide = 1,

    /// Spacer after wide character. Do not render.
    SpacerTail = 2,

    /// Spacer at the end of a soft-wrapped line to indicate that a wide
    /// character is continued on the next line.
    SpacerHead = 3,
}

/// (Zig: `Cell.SemanticContent`, `enum(u2)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum SemanticContent {
    /// Regular output content, such as command output.
    #[default]
    Output = 0,

    /// Content that is part of user input, such as the command
    /// to execute at a prompt.
    Input = 1,

    /// Content that is part of prompt emitted by the interactive
    /// application, such as "user@host >"
    Prompt = 2,
}

/// A cell represents a single terminal grid cell (Zig: `Cell`, a `packed
/// struct(u64)`).
///
/// The zero value of this struct must be a valid cell representing empty,
/// since we zero initialize the backing memory for a page.
///
/// See [`cell_fields`] for the exact bit layout.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Cell(u64);

/// The backing integer of [`Cell`] (Zig: `Cell.Backing`).
pub type CellBacking = u64;

/// C ABI type of [`Cell`] (Zig: `Cell.C`).
pub type CellC = u64;

/// Bit layout metadata for the C representation of [`Cell`]
/// (Zig: `Cell.CLayout`). All physical bit offsets and widths mirror
/// [`cell_fields`]; Zig reflects them from the packed struct, the port
/// declares them once with [`crate::lib::packed::packed_fields`].
pub mod cell_c_layout {
    use crate::lib::packed::{
        packed_fields, Arm, Encoding, FieldSpec, PackedField, PackedLayout, PackedTaggedUnion,
    };

    // `content.codepoint`: packed struct(u24) { data: u21, _pad: u3 }
    static CODEPOINT_FIELDS: [PackedField; 2] = packed_fields([
        FieldSpec::scalar("data", "u21", 21).name("codepoint"),
        FieldSpec::scalar("_pad", "u3", 3).omit(),
    ]);
    /// Layout of the `codepoint` arm of `Cell.content`.
    pub static CODEPOINT: PackedLayout = PackedLayout::new("Codepoint", 24, &CODEPOINT_FIELDS);

    // `content.color_palette`: packed struct(u24) { data: u8, _pad: u16 }
    static COLOR_PALETTE_FIELDS: [PackedField; 2] = packed_fields([
        FieldSpec::scalar("data", "u8", 8)
            .name("index")
            .type_name("GhosttyColorPaletteIndex"),
        FieldSpec::scalar("_pad", "u16", 16).omit(),
    ]);
    /// Layout of the `color_palette` arm of `Cell.content`.
    pub static COLOR_PALETTE: PackedLayout =
        PackedLayout::new("ColorPalette", 24, &COLOR_PALETTE_FIELDS);

    // `content.color_rgb`: Cell.RGB = packed struct { r: u8, g: u8, b: u8 }
    static COLOR_RGB_FIELDS: [PackedField; 3] = packed_fields([
        FieldSpec::scalar("r", "u8", 8),
        FieldSpec::scalar("g", "u8", 8),
        FieldSpec::scalar("b", "u8", 8),
    ]);
    /// Layout of the `color_rgb` arm of `Cell.content`.
    pub static COLOR_RGB: PackedLayout = PackedLayout::new("RGB", 24, &COLOR_RGB_FIELDS);

    /// The `content` packed union, selected by `content_tag`.
    pub static CONTENT: PackedTaggedUnion = PackedTaggedUnion::new(
        "Cell",
        "Content",
        24,
        "content",
        "content_tag",
        &[
            (
                "codepoint",
                Some(Arm {
                    source: "codepoint",
                    source_ty: "Codepoint",
                    layout: &CODEPOINT,
                }),
            ),
            (
                "codepoint_grapheme",
                Some(Arm {
                    source: "codepoint",
                    source_ty: "Codepoint",
                    layout: &CODEPOINT,
                }),
            ),
            (
                "bg_color_palette",
                Some(Arm {
                    source: "color_palette",
                    source_ty: "ColorPalette",
                    layout: &COLOR_PALETTE,
                }),
            ),
            (
                "bg_color_rgb",
                Some(Arm {
                    source: "color_rgb",
                    source_ty: "RGB",
                    layout: &COLOR_RGB,
                }),
            ),
        ],
    );

    static FIELDS: [PackedField; 8] = packed_fields([
        FieldSpec::scalar("content_tag", "ContentTag", 2).type_name("GhosttyCellContentTag"),
        FieldSpec::compound("content", "Content", 24).encoding(Encoding::TaggedUnion(&CONTENT)),
        FieldSpec::scalar("style_id", "u16", 16).type_name("GhosttyStyleId"),
        FieldSpec::scalar("wide", "Wide", 2).type_name("GhosttyCellWide"),
        FieldSpec::scalar("protected", "bool", 1),
        FieldSpec::scalar("hyperlink", "bool", 1),
        FieldSpec::scalar("semantic_content", "SemanticContent", 2)
            .type_name("GhosttyCellSemanticContent"),
        FieldSpec::scalar("_padding", "u16", 16).omit(),
    ]);

    /// Metadata for the C representation (Zig: `Cell.CLayout`).
    pub static LAYOUT: PackedLayout = PackedLayout::new("Cell", 64, &FIELDS);

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::terminal::page::cell_fields;

        #[test]
        fn c_layout_matches_cell_fields() {
            let check = |name: &str, f: crate::terminal::page::BitField| {
                assert_eq!(f.offset as usize, LAYOUT.bit_offset(name), "{name}");
                assert_eq!(f.bits as usize, LAYOUT.bit_width(name), "{name}");
            };
            check("content_tag", cell_fields::CONTENT_TAG);
            check("content", cell_fields::CONTENT);
            check("style_id", cell_fields::STYLE_ID);
            check("wide", cell_fields::WIDE);
            check("protected", cell_fields::PROTECTED);
            check("hyperlink", cell_fields::HYPERLINK);
            check("semantic_content", cell_fields::SEMANTIC_CONTENT);
            check("_padding", cell_fields::PADDING);
            assert!(LAYOUT.field_name("_padding").is_none());
            assert_eq!(Some("codepoint"), CODEPOINT.field_name("data"));
            assert_eq!(Some("index"), COLOR_PALETTE.field_name("data"));
        }
    }
}

impl Cell {
    #[inline(always)]
    pub const fn from_bits(bits: u64) -> Cell {
        Cell(bits)
    }

    #[inline(always)]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// Returns this cell as a C ABI value.
    #[inline]
    pub fn cval(self) -> CellC {
        self.0
    }

    /// Helper to make a cell that just has a codepoint.
    #[inline]
    pub const fn init(cp: u32) -> Cell {
        // All other bits are zero.
        Cell(cell_fields::CONTENT_CODEPOINT_DATA.set(0, cp as u64))
    }

    /// The content tag dictates the active tag in content and possibly
    /// some other behaviors.
    #[inline(always)]
    pub fn content_tag(&self) -> ContentTag {
        match cell_fields::CONTENT_TAG.get(self.0) {
            0 => ContentTag::Codepoint,
            1 => ContentTag::CodepointGrapheme,
            2 => ContentTag::BgColorPalette,
            _ => ContentTag::BgColorRgb,
        }
    }

    #[inline(always)]
    pub fn set_content_tag(&mut self, v: ContentTag) {
        self.0 = cell_fields::CONTENT_TAG.set(self.0, v as u64);
    }

    /// The raw 24 bits of the content packed union.
    #[inline(always)]
    pub fn content_raw(&self) -> u32 {
        cell_fields::CONTENT.get(self.0) as u32
    }

    #[inline(always)]
    pub fn set_content_raw(&mut self, v: u32) {
        self.0 = cell_fields::CONTENT.set(self.0, v as u64);
    }

    /// `content.codepoint.data`: the codepoint that this cell contains.
    /// If `grapheme` is false, then this is the only codepoint in the cell.
    /// If `grapheme` is true, then this is the first codepoint in the
    /// grapheme cluster.
    #[inline(always)]
    pub fn content_codepoint(&self) -> u32 {
        cell_fields::CONTENT_CODEPOINT_DATA.get(self.0) as u32
    }

    /// Sets the content to the codepoint arm (Zig:
    /// `.content = .{ .codepoint = .{ .data = cp } }`; the padding bits of
    /// the content are zeroed).
    #[inline(always)]
    pub fn set_content_codepoint(&mut self, cp: u32) {
        self.set_content_raw(cp & 0x1F_FFFF);
    }

    /// `content.color_palette.data`: the content is an empty cell with a
    /// background color.
    #[inline(always)]
    pub fn content_color_palette(&self) -> u8 {
        cell_fields::CONTENT_COLOR_PALETTE_DATA.get(self.0) as u8
    }

    /// Sets the content to the color_palette arm (padding zeroed).
    #[inline(always)]
    pub fn set_content_color_palette(&mut self, idx: u8) {
        self.set_content_raw(idx as u32);
    }

    /// `content.color_rgb`.
    #[inline(always)]
    pub fn content_color_rgb(&self) -> Rgb {
        Rgb::from_bits(cell_fields::CONTENT_COLOR_RGB.get(self.0) as u32)
    }

    /// Sets the content to the color_rgb arm.
    #[inline(always)]
    pub fn set_content_color_rgb(&mut self, rgb: Rgb) {
        self.set_content_raw(rgb.to_bits());
    }

    /// The style ID to use for this cell within the style map. Zero
    /// is always the default style so no lookup is required.
    #[inline(always)]
    pub fn style_id(&self) -> StyleId {
        cell_fields::STYLE_ID.get(self.0) as StyleId
    }

    #[inline(always)]
    pub fn set_style_id(&mut self, v: StyleId) {
        self.0 = cell_fields::STYLE_ID.set(self.0, v as u64);
    }

    /// The wide property of this cell, for wide characters. Characters in
    /// a terminal grid can only be 1 or 2 cells wide. A wide character
    /// is always next to a spacer. This is used to determine both the width
    /// and spacer properties of a cell.
    #[inline(always)]
    pub fn wide(&self) -> Wide {
        match cell_fields::WIDE.get(self.0) {
            0 => Wide::Narrow,
            1 => Wide::Wide,
            2 => Wide::SpacerTail,
            _ => Wide::SpacerHead,
        }
    }

    #[inline(always)]
    pub fn set_wide(&mut self, v: Wide) {
        self.0 = cell_fields::WIDE.set(self.0, v as u64);
    }

    // Whether this was written with the protection flag set.
    bool_field!(cell_fields, protected, set_protected, PROTECTED);

    // Whether this cell is a hyperlink. If this is true then you must
    // look up the hyperlink ID in the page hyperlink_map and the ID in
    // the hyperlink_set to get the actual hyperlink data.
    bool_field!(cell_fields, hyperlink, set_hyperlink, HYPERLINK);

    /// The semantic type of the content of this cell. This is used
    /// by the semantic prompt (OSC 133) set of sequences to understand
    /// boundary points for content.
    #[inline(always)]
    pub fn semantic_content(&self) -> SemanticContent {
        match cell_fields::SEMANTIC_CONTENT.get(self.0) {
            0 => SemanticContent::Output,
            1 => SemanticContent::Input,
            2 => SemanticContent::Prompt,
            // Not a valid value of the enum; Zig would treat this as
            // illegal behavior.
            _ => unreachable!("invalid SemanticContent bits"),
        }
    }

    #[inline(always)]
    pub fn set_semantic_content(&mut self, v: SemanticContent) {
        self.0 = cell_fields::SEMANTIC_CONTENT.set(self.0, v as u64);
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    /// Returns true if this cell represents a cell with text to render.
    ///
    /// Cases this returns false:
    ///   - Cell text is blank
    ///   - Cell is styled but only with a background color and no text
    ///   - Cell has a unicode placeholder for Kitty graphics protocol
    #[inline]
    pub fn has_text(&self) -> bool {
        match self.content_tag() {
            ContentTag::Codepoint | ContentTag::CodepointGrapheme => self.content_codepoint() != 0,
            ContentTag::BgColorPalette | ContentTag::BgColorRgb => false,
        }
    }

    #[inline]
    pub fn codepoint(&self) -> u32 {
        match self.content_tag() {
            ContentTag::Codepoint | ContentTag::CodepointGrapheme => self.content_codepoint(),
            ContentTag::BgColorPalette | ContentTag::BgColorRgb => 0,
        }
    }

    /// The width in grid cells that this cell takes up.
    #[inline]
    pub fn grid_width(&self) -> u8 {
        match self.wide() {
            Wide::Narrow | Wide::SpacerHead | Wide::SpacerTail => 1,
            Wide::Wide => 2,
        }
    }

    #[inline]
    pub fn has_styling(&self) -> bool {
        self.style_id() != style::DEFAULT_ID
    }

    /// Returns true if the cell has no text or styling.
    pub fn is_empty(&self) -> bool {
        match self.content_tag() {
            // Textual cells are empty if they have no text and are narrow.
            // The "narrow" requirement is because wide spacers are meaningful.
            ContentTag::Codepoint | ContentTag::CodepointGrapheme => !self.has_text() && self.wide() == Wide::Narrow,

            ContentTag::BgColorPalette | ContentTag::BgColorRgb => false,
        }
    }

    #[inline]
    pub fn has_grapheme(&self) -> bool {
        self.content_tag() == ContentTag::CodepointGrapheme
    }

    /// Returns true if the set of cells has text in it.
    #[inline]
    pub fn has_text_any(cells: &[Cell]) -> bool {
        cells.iter().any(|cell| cell.has_text())
    }
}

impl Packed for Cell {
    #[inline(always)]
    fn to_bits(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cell")
            .field("content_tag", &self.content_tag())
            .field("content", &format_args!("{:#x}", self.content_raw()))
            .field("style_id", &self.style_id())
            .field("wide", &self.wide())
            .field("protected", &self.protected())
            .field("hyperlink", &self.hyperlink())
            .field("semantic_content", &cell_fields::SEMANTIC_CONTENT.get(self.0))
            .finish()
    }
}

/// Returns a mask with all bits set for the given fields of a packed
/// struct, used for masked compares of raw backing-integer values
/// (e.g. `Row.Backing`, `Cell.Backing`). This is an implementation
/// detail of `Mask`, which is the public API built on top of this.
///
/// A field may be a nested field (e.g. [`cell_fields::CONTENT_CODEPOINT_DATA`])
/// to cover only a nested field of a packed struct or packed union
/// member. This allows a mask to be more precise than a whole
/// top-level field, e.g. covering the codepoint bits of a cell without
/// its padding.
pub const fn field_mask(fields: &[BitField]) -> u64 {
    let mut mask: u64 = 0;
    let mut i = 0;
    while i < fields.len() {
        mask |= fields[i].mask();
        i += 1;
    }
    mask
}

/// A helper for classifying and comparing packed struct values (e.g. Row,
/// Cell) in bulk, using masked compares of their raw backing integers.
///
/// Masked compares are the key to making bulk row/cell processing fast.
/// Rows and cells are small packed structs specifically so that a single
/// integer load observes every field at once. A masked compare can then
/// answer a multi-field question with one AND and one compare, instead
/// of extracting and branching on each field individually (each packed
/// field access compiles to its own shift/mask). Just as importantly,
/// the integer form vectorizes trivially: splat the mask and expected
/// value, and whole groups of rows or cells can be classified with a
/// few SIMD instructions.
///
/// Some real examples of this in use:
///
///   - Terminal print fast path: a cell can be overwritten by the
///     simple/fast path only if its content tag is a plain codepoint,
///     it has no style, isn't wide, and isn't a hyperlink. Masking
///     with those fields and comparing against a template answers all
///     four questions in one compare per cell.
///
///   - Render state updates: a cell whose masked
///     `{content_tag, style_id}` bits are zero is a plain cell that
///     needs no managed-memory handling, so a vector OR-reduce can
///     skip entire groups of plain cells at once. Similarly, cells
///     whose masked bits equal the first cell's form a run sharing one
///     style, letting the update record one style lookup per run
///     rather than per cell.
///
///   - Dirty scans: OR-reducing groups of rows against the `dirty`
///     field mask finds whether any row in the group needs a rebuild
///     without touching each row's flag individually.
///
/// T is the packed struct type, MASK is the field mask (see
/// [`field_mask`]), and G is the number of values processed at once by
/// the group (vectorized) operations. Callers typically scan a slice
/// with the group operations and fall back to the scalar variants for
/// the remainder and for pinpointing values within a matched group.
pub struct Mask<T, const MASK: u64, const G: usize>(PhantomData<T>);

impl<T: Packed, const MASK: u64, const G: usize> Mask<T, MASK, G> {
    const MASK_VALUE: u64 = MASK;

    /// The number of values processed at once by group operations.
    pub const GROUP_LEN: usize = G;

    /// Load a group of values from the slice starting at index i.
    /// Asserts that at least group_len values are available.
    #[inline(always)]
    fn load(values: &[T], i: usize) -> [u64; G] {
        let group = &values[i..i + G];
        core::array::from_fn(|j| group[j].to_bits())
    }

    /// Returns the raw backing bits of a single value.
    #[inline(always)]
    pub fn bits(v: T) -> u64 {
        v.to_bits()
    }

    /// Returns the masked bits of a single value: the bits of the
    /// masked fields with all other fields zeroed. Use this to
    /// build the expected value for the eql functions.
    #[inline(always)]
    pub fn pattern(v: T) -> u64 {
        Self::bits(v) & Self::MASK_VALUE
    }

    /// Returns the backing bits of a single value with the masked
    /// fields zeroed: the complement of `pattern`. Use this to
    /// compare values while ignoring the masked fields.
    #[inline(always)]
    pub fn strip(v: T) -> u64 {
        Self::bits(v) & !Self::MASK_VALUE
    }

    /// Returns true if every value in the group of group_len
    /// values starting at index i matches, where a value matches
    /// when none of the masked fields have any bits set: false
    /// for bools, zero for ints, the zero tag for enums, and so
    /// on. Asserts that at least group_len values are available.
    #[inline(always)]
    pub fn r#match(values: &[T], i: usize) -> bool {
        Self::load(values, i).iter().fold(0u64, |acc, v| acc | v) & Self::MASK_VALUE == 0
    }

    /// Scalar variant of `match` for a single value.
    #[inline(always)]
    pub fn match_scalar(v: T) -> bool {
        Self::bits(v) & Self::MASK_VALUE == 0
    }

    /// Returns true if the masked fields of every value in the
    /// group of group_len values starting at index i equal the
    /// expected pattern (see `pattern`).
    ///
    /// This is a masked compare: fields outside the mask may vary
    /// freely. Use this to detect runs of values that share the
    /// masked field contents while other fields differ, e.g. a run
    /// of cells with the same style ID but different codepoints.
    /// If the result you derive from a run depends on fields
    /// outside the mask, use `eqlExact` instead.
    #[inline(always)]
    pub fn eql(values: &[T], i: usize, expected: u64) -> bool {
        Self::load(values, i).iter().all(|v| v & Self::MASK_VALUE == expected)
    }

    /// Scalar variant of `eql` for a single value.
    #[inline(always)]
    pub fn eql_scalar(v: T, expected: u64) -> bool {
        Self::pattern(v) == expected
    }

    /// Returns true if any value in the group of group_len values
    /// starting at index i has masked fields equal to the expected
    /// pattern (see `pattern`). This is the "any" counterpart to
    /// `eql`: use it to detect the presence of a specific value
    /// within a group, e.g. a run scan that must stop when it
    /// encounters a sentinel codepoint anywhere in the group.
    #[inline(always)]
    pub fn eql_any(values: &[T], i: usize, expected: u64) -> bool {
        Self::load(values, i).iter().any(|v| v & Self::MASK_VALUE == expected)
    }

    /// Like `eql` but returns the number of leading values whose
    /// masked fields equal the expected pattern, i.e. group_len if
    /// the entire group matches. This is useful for early-exit run
    /// scans that need to pinpoint exactly where a run ends rather
    /// than only whether the whole group matches.
    #[inline(always)]
    pub fn eql_prefix(values: &[T], i: usize, expected: u64) -> usize {
        let group = Self::load(values, i);
        group
            .iter()
            .position(|v| v & Self::MASK_VALUE != expected)
            .unwrap_or(G)
    }

    /// Returns true if every value in the group of group_len
    /// values starting at index i is bit-identical to the expected
    /// value. Note: this compares entire values; it is NOT
    /// affected by the field mask.
    ///
    /// This exists alongside `eql` for run detection where the
    /// derived result depends on fields outside the mask, so a
    /// masked compare would incorrectly extend the run. For
    /// example, the background color of a bg-color cell lives in
    /// the content field: two such cells only share a background
    /// if their content bits are identical, not merely their
    /// content tag. Values in such runs are typically produced by
    /// bulk fills (e.g. erase with a pending background) and are
    /// bit-identical in practice, so exact equality is both
    /// correct and cheap.
    #[inline(always)]
    pub fn eql_exact(values: &[T], i: usize, expected: u64) -> bool {
        Self::load(values, i).iter().all(|&v| v == expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::hyperlink::{Hyperlink, HyperlinkId};
    use std::borrow::Cow;
    use std::ops::{Deref, DerefMut};

    /// A page that is deinit'ed on drop (Zig: `defer page.deinit()`).
    struct TestPage(Page);

    impl Deref for TestPage {
        type Target = Page;
        fn deref(&self) -> &Page {
            &self.0
        }
    }

    impl DerefMut for TestPage {
        fn deref_mut(&mut self) -> &mut Page {
            &mut self.0
        }
    }

    impl Drop for TestPage {
        fn drop(&mut self) {
            self.0.deinit();
        }
    }

    fn init(cap: Capacity) -> TestPage {
        TestPage(Page::init(cap).unwrap())
    }

    fn cap3(cols: u16, rows: u16, styles: u16) -> Capacity {
        Capacity {
            styles,
            ..Capacity::new(cols, rows)
        }
    }

    /// Zig: `page.getRowAndCell(x, y)` with `rac.row`/`rac.cell` used as
    /// pointers. Test-only: the references have an unbounded lifetime.
    fn rac<'a>(page: &Page, x: usize, y: usize) -> (&'a mut Row, &'a mut Cell) {
        let r = page.get_row_and_cell(x, y);
        unsafe { (&mut *r.row, &mut *r.cell) }
    }

    fn row<'a>(page: &Page, y: usize) -> &'a mut Row {
        unsafe { &mut *page.get_row(y) }
    }

    fn cp_cell(cp: u32) -> Cell {
        let mut c = Cell::default();
        c.set_content_tag(ContentTag::Codepoint);
        c.set_content_codepoint(cp);
        c
    }

    fn bold() -> Style {
        let mut s = Style::default();
        s.flags.bold = true;
        s
    }

    fn link<'a>(id: HyperlinkId<'a>, uri: &'a [u8]) -> Hyperlink<'a> {
        Hyperlink {
            id,
            uri: Cow::Borrowed(uri),
        }
    }

    type M1 = Mask<Cell, { field_mask(&[cell_fields::CONTENT_TAG, cell_fields::STYLE_ID]) }, 4>;
    type M2 = Mask<Cell, { field_mask(&[cell_fields::CONTENT_CODEPOINT_DATA]) }, 4>;

    // The page layout must match Zig's byte for byte: it decides how many
    // rows a page of a given width holds (and so scrollback memory and the
    // page records of snapshots). Zig: `Page.layout(std_capacity)` with
    // the non-test grapheme capacity is 401408 bytes and adjusting it to
    // 215 columns gives 217 rows.
    #[test]
    fn std_capacity_layout_matches_zig() {
        let cap = Capacity {
            grapheme_bytes: 8192,
            ..STD_CAPACITY
        };
        let meta = MetaLayout::init(cap);
        assert_eq!(4000, meta.styles_layout.total_size);
        assert_eq!(26124, meta.total_size);
        assert_eq!(401408, Page::layout(cap).total_size);
        let adjusted = cap.adjust(Adjustment { cols: Some(215) }).unwrap();
        assert_eq!(217, adjusted.rows);
    }

    #[test]
    fn mask() {
        type M = M1;

        let plain = Cell::init('A' as u32);
        let mut styled = Cell::init('B' as u32);
        styled.set_style_id(5);
        let mut styled2 = Cell::init('C' as u32);
        styled2.set_style_id(5);
        let mut other = Cell::init('D' as u32);
        other.set_style_id(6);

        // match: plain cells only
        {
            let mut cells = [plain, plain, plain, plain];
            assert!(M::r#match(&cells, 0));
            assert!(M::match_scalar(plain));

            cells[2] = styled;
            assert!(!M::r#match(&cells, 0));
            assert!(!M::match_scalar(styled));
        }

        // eql: runs of matching masked fields, other fields may vary
        {
            let expected = M::pattern(styled);
            let mut cells = [styled, styled2, styled, styled2];
            assert!(M::eql(&cells, 0, expected));
            assert!(M::eql_scalar(styled2, expected));

            cells[1] = other;
            assert!(!M::eql(&cells, 0, expected));
            assert!(!M::eql_scalar(other, expected));
        }

        // eqlPrefix: count of leading values matching the pattern
        {
            let expected = M::pattern(styled);
            let mut cells = [styled, styled2, other, styled];
            assert_eq!(2, M::eql_prefix(&cells, 0, expected));

            cells[2] = styled;
            assert_eq!(4, M::eql_prefix(&cells, 0, expected));
        }

        // eqlExact: bit-identical values only
        {
            let expected = M::bits(styled);
            let mut cells = [styled, styled, styled, styled];
            assert!(M::eql_exact(&cells, 0, expected));

            // Same masked fields but different codepoint is not exact.
            cells[3] = styled2;
            assert!(!M::eql_exact(&cells, 0, expected));
        }

        // strip: compare values while ignoring the masked fields
        {
            let mut styled_other = Cell::init('B' as u32);
            styled_other.set_style_id(6);
            assert_eq!(M::strip(styled), M::strip(styled_other));
            assert!(M::strip(styled) != M::strip(styled2));
        }

        // eqlAny: presence of a matching value anywhere in the group
        {
            let expected = M::pattern(styled);
            let mut cells = [plain, plain, plain, plain];
            assert!(!M::eql_any(&cells, 0, expected));

            cells[2] = styled;
            assert!(M::eql_any(&cells, 0, expected));

            // Masked compare: same masked fields with a different
            // codepoint still matches.
            cells[2] = styled2;
            assert!(M::eql_any(&cells, 0, expected));
        }

        assert_eq!(M::GROUP_LEN, 4);
    }

    #[test]
    fn mask_nested_field_path() {
        // Mask only the codepoint data bits of the content field, not
        // the padding next to it or any other field.
        type M = M2;

        let a = Cell::init('A' as u32);
        let mut b = Cell::init('A' as u32);
        b.set_style_id(5);
        b.set_wide(Wide::Wide);
        let c = Cell::init('C' as u32);

        // Same codepoint matches regardless of other fields.
        let expected = M::pattern(a);
        assert!(M::eql_scalar(b, expected));
        assert!(!M::eql_scalar(c, expected));

        // The mask must cover exactly the codepoint data bits.
        let cp_offset = cell_fields::CONTENT.offset;
        assert_eq!(
            (0x1F_FFFFu64) << cp_offset,
            field_mask(&[cell_fields::CONTENT_CODEPOINT_DATA])
        );

        // Group variants
        {
            let mut cells = [a, b, a, b];
            assert!(M::eql(&cells, 0, expected));
            assert!(M::eql_any(&cells, 0, expected));

            cells[1] = c;
            assert!(!M::eql(&cells, 0, expected));
            assert!(M::eql_any(&cells, 0, expected));

            let none = [c, c, c, c];
            assert!(!M::eql_any(&none, 0, expected));
        }
    }

    #[test]
    fn row_and_cell_bit_layout() {
        // Zig: `packed struct(u64)` sizes.
        assert_eq!(core::mem::size_of::<Row>(), 8);
        assert_eq!(core::mem::size_of::<Cell>(), 8);

        let mut r = Row::init(Offset::new(0x1234_5678));
        assert_eq!(r.to_bits(), 0x1234_5678);
        r.set_wrap(true);
        assert_eq!(r.to_bits(), 0x1234_5678 | 1 << 32);
        r.set_semantic_prompt(SemanticPrompt::PromptContinuation);
        assert_eq!(r.to_bits() >> 37 & 0b11, 2);
        r.set_dirty(true);
        assert_eq!(r.to_bits() >> 40 & 1, 1);

        let mut c = Cell::init(0x10FFFF);
        assert_eq!(c.to_bits(), 0x10FFFF << 2);
        c.set_style_id(0xFFFF);
        assert_eq!(c.to_bits() >> 26 & 0xFFFF, 0xFFFF);
        c.set_wide(Wide::SpacerHead);
        assert_eq!(c.to_bits() >> 42 & 0b11, 3);
        c.set_semantic_content(SemanticContent::Prompt);
        assert_eq!(c.to_bits() >> 46 & 0b11, 2);
        c.set_content_color_rgb(Rgb::new(1, 2, 3));
        assert_eq!(c.content_raw(), 0x030201);
    }

    #[test]
    fn page_layout_can_take_a_maxed_capacity() {
        // Our intention is for a maxed-out capacity to always fit
        // within a page layout without triggering runtime safety on any
        // overflow. This simplifies some of our handling downstream of the
        // call (relevant to: https://github.com/ghostty-org/ghostty/issues/10258)
        let cap = Capacity {
            cols: u16::MAX,
            rows: u16::MAX,
            styles: u16::MAX,
            hyperlink_bytes: u16::MAX,
            grapheme_bytes: u32::MAX,
            string_bytes: u32::MAX,
        };

        // Note that a max capacity will exceed our max_page_size so we
        // can't init a page with it, but it should layout.
        let _ = Page::layout(cap);
    }

    #[test]
    fn cell_is_zero_by_default() {
        let cell = Cell::init(0);
        let cell_int: u64 = cell.to_bits();
        assert_eq!(0u64, cell_int);

        // The zero value should be output type for semantic content.
        // This is very important for our assumptions elsewhere.
        assert_eq!(SemanticContent::Output, cell.semantic_content());
    }

    #[test]
    fn page_capacity_adjust_cols_down() {
        let original = STD_CAPACITY;
        let original_size = Page::layout(original).total_size;
        let adjusted = original
            .adjust(Adjustment {
                cols: Some(original.cols / 2),
            })
            .unwrap();
        let adjusted_size = Page::layout(adjusted).total_size;
        assert_eq!(original_size, adjusted_size);
        // If we layout a page with 1 more row and it's still the same size
        // then adjust is not producing enough rows.
        let mut bigger = adjusted;
        bigger.rows += 1;
        let bigger_size = Page::layout(bigger).total_size;
        assert!(bigger_size > original_size);
    }

    #[test]
    fn page_capacity_adjust_cols_down_to_1() {
        let original = STD_CAPACITY;
        let original_size = Page::layout(original).total_size;
        let adjusted = original.adjust(Adjustment { cols: Some(1) }).unwrap();
        let adjusted_size = Page::layout(adjusted).total_size;
        assert_eq!(original_size, adjusted_size);
        // If we layout a page with 1 more row and it's still the same size
        // then adjust is not producing enough rows.
        let mut bigger = adjusted;
        bigger.rows += 1;
        let bigger_size = Page::layout(bigger).total_size;
        assert!(bigger_size > original_size);
    }

    #[test]
    fn page_capacity_adjust_cols_up() {
        let original = STD_CAPACITY;
        let original_size = Page::layout(original).total_size;
        let adjusted = original
            .adjust(Adjustment {
                cols: Some(original.cols * 2),
            })
            .unwrap();
        let adjusted_size = Page::layout(adjusted).total_size;
        assert_eq!(original_size, adjusted_size);
        // If we layout a page with 1 more row and it's still the same size
        // then adjust is not producing enough rows.
        let mut bigger = adjusted;
        bigger.rows += 1;
        let bigger_size = Page::layout(bigger).total_size;
        assert!(bigger_size > original_size);
    }

    #[test]
    fn page_capacity_adjust_cols_sweep() {
        let mut cap = STD_CAPACITY;
        let original_cols = cap.cols;
        let original_size = Page::layout(cap).total_size;
        for c in 1..(original_cols as usize * 2) {
            cap = cap.adjust(Adjustment { cols: Some(c as u16) }).unwrap();
            let adjusted_size = Page::layout(cap).total_size;
            assert_eq!(original_size, adjusted_size);
            // If we layout a page with 1 more row and it's still the same size
            // then adjust is not producing enough rows.
            let mut bigger = cap;
            bigger.rows += 1;
            let bigger_size = Page::layout(bigger).total_size;
            assert!(bigger_size > original_size);
        }
    }

    #[test]
    fn page_capacity_adjust_cols_too_high() {
        let original = STD_CAPACITY;
        assert_eq!(
            Err(AllocError::OutOfMemory),
            original.adjust(Adjustment {
                cols: Some(size::CellCountInt::MAX)
            })
        );
    }

    #[test]
    fn capacity_max_cols_basic() {
        let cap = STD_CAPACITY;
        let max = cap.max_cols().unwrap();

        // maxCols should be >= current cols (since current capacity is valid)
        assert!(max >= cap.cols);

        // Adjusting to maxCols should succeed with at least 1 row
        let adjusted = cap.adjust(Adjustment { cols: Some(max) }).unwrap();
        assert!(adjusted.rows >= 1);

        // Adjusting to maxCols + 1 should fail
        assert_eq!(
            Err(AllocError::OutOfMemory),
            cap.adjust(Adjustment { cols: Some(max + 1) })
        );
    }

    #[test]
    fn capacity_max_cols_preserves_total_size() {
        let cap = STD_CAPACITY;
        let original_size = Page::layout(cap).total_size;
        let max = cap.max_cols().unwrap();
        let adjusted = cap.adjust(Adjustment { cols: Some(max) }).unwrap();
        let adjusted_size = Page::layout(adjusted).total_size;
        assert_eq!(original_size, adjusted_size);
    }

    #[test]
    fn capacity_max_cols_with_1_row_exactly() {
        let cap = STD_CAPACITY;
        let max = cap.max_cols().unwrap();
        let adjusted = cap.adjust(Adjustment { cols: Some(max) }).unwrap();
        assert_eq!(1 as size::CellCountInt, adjusted.rows);
    }

    #[test]
    fn capacity_adjust_clamps_rows_to_cell_count_int() {
        // A max-width page narrowed to one column frees enough memory for far
        // more rows than a page can hold. The row count must be capped, and
        // the page must still fit in the original memory size.
        let cap = Capacity {
            cols: size::CellCountInt::MAX,
            rows: 4,
            styles: 0,
            grapheme_bytes: 0,
            hyperlink_bytes: 0,
            string_bytes: 0,
        };
        let adjusted = cap.adjust(Adjustment { cols: Some(1) }).unwrap();
        assert_eq!(size::CellCountInt::MAX, adjusted.rows);
        assert!(Page::layout(adjusted).total_size <= Page::layout(cap).total_size);
    }

    #[test]
    fn page_init() {
        let _page = init(cap3(120, 80, 32));
    }

    #[test]
    fn page_read_and_write_cells() {
        let page = init(cap3(10, 10, 8));

        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            *cell = cp_cell(y as u32);
        }

        // Read it again
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            assert_eq!(y as u32, cell.content_codepoint());
        }
    }

    #[test]
    fn page_append_grapheme_small() {
        let mut page = init(cap3(10, 10, 8));

        let (row, cell) = rac(&page, 0, 0);
        *cell = Cell::init(0x09);

        // One
        page.append_grapheme(row, cell, 0x0A).unwrap();
        assert!(row.grapheme());
        assert!(cell.has_grapheme());
        assert_eq!(&[0x0A][..], page.lookup_grapheme(cell).unwrap());

        // Two
        page.append_grapheme(row, cell, 0x0B).unwrap();
        assert!(row.grapheme());
        assert!(cell.has_grapheme());
        assert_eq!(&[0x0A, 0x0B][..], page.lookup_grapheme(cell).unwrap());

        // Clear it
        page.clear_grapheme(cell);
        page.update_row_grapheme_flag(row);
        assert!(!row.grapheme());
        assert!(!cell.has_grapheme());
    }

    #[test]
    fn page_append_grapheme_larger_than_chunk() {
        let mut page = init(cap3(10, 10, 8));

        let (row, cell) = rac(&page, 0, 0);
        *cell = Cell::init(0x09);

        let count = GRAPHEME_CHUNK_LEN * 10;
        for i in 0..count {
            page.append_grapheme(row, cell, 0x0A + i as u32).unwrap();
        }

        let cps = page.lookup_grapheme(cell).unwrap();
        assert_eq!(count, cps.len());
        for (i, &cp) in cps.iter().enumerate() {
            assert_eq!(0x0A + i as u32, cp);
        }
    }

    #[test]
    fn page_append_grapheme_caps_codepoints_per_cell() {
        let mut page = init(cap3(10, 10, 8));

        let (row, cell) = rac(&page, 0, 0);
        *cell = Cell::init('A' as u32);

        for i in 0..GRAPHEME_MAX_LEN + 16 {
            page.append_grapheme(row, cell, 0x0300 + i as u32).unwrap();
        }

        let cps = page.lookup_grapheme(cell).unwrap();
        assert_eq!(GRAPHEME_MAX_LEN, cps.len());
        for (i, &cp) in cps.iter().enumerate() {
            assert_eq!(0x0300 + i as u32, cp);
        }
    }

    #[test]
    fn page_set_graphemes_caps_codepoints_per_cell() {
        let mut page = init(cap3(10, 10, 8));

        let mut input = [0u32; GRAPHEME_MAX_LEN + 16];
        for (i, cp) in input.iter_mut().enumerate() {
            *cp = 0x0300 + i as u32;
        }

        let (row, cell) = rac(&page, 0, 0);
        *cell = Cell::init('A' as u32);
        page.set_graphemes(row, cell, &input).unwrap();

        assert_eq!(GRAPHEME_MAX_LEN, page.lookup_grapheme(cell).unwrap().len());
    }

    #[test]
    fn page_clear_grapheme_not_all_cells() {
        let mut page = init(cap3(10, 10, 8));

        let (row, cell) = rac(&page, 0, 0);
        *cell = Cell::init(0x09);
        page.append_grapheme(row, cell, 0x0A).unwrap();

        let (row2, cell2) = rac(&page, 1, 0);
        *cell2 = Cell::init(0x09);
        page.append_grapheme(row2, cell2, 0x0A).unwrap();

        // Clear it
        page.clear_grapheme(cell);
        page.update_row_grapheme_flag(row);
        assert!(row.grapheme());
        assert!(!cell.has_grapheme());
        assert!(cell2.has_grapheme());
    }

    #[test]
    fn page_clear_cells_full_row_clears_kitty_placeholder_flag() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut page = init(cap3(10, 10, 8));

        let (row, cell) = rac(&page, 0, 0);
        *cell = Cell::init(graphics_unicode::PLACEHOLDER);
        row.set_kitty_virtual_placeholder(true);

        let cols = page.size.cols as usize;
        page.clear_cells(row, 0, cols);
        assert!(!row.kitty_virtual_placeholder());
    }

    #[test]
    fn page_clone() {
        let page = init(cap3(10, 10, 8));

        // Write
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            *cell = cp_cell(y as u32);
        }

        // Clone
        let page2 = TestPage(page.clone().unwrap());
        assert_eq!(page2.capacity, page.capacity);

        // Read it again
        for y in 0..page2.capacity.rows as usize {
            let (_, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32, cell.content_codepoint());
        }

        // Write again
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            *cell = cp_cell(0);
        }

        // Read it again, should be unchanged
        for y in 0..page2.capacity.rows as usize {
            let (_, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32, cell.content_codepoint());
        }

        // Read the original
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            assert_eq!(0, cell.content_codepoint());
        }
    }

    #[test]
    fn page_clone_graphemes() {
        let mut page = init(cap3(10, 10, 8));

        // Append some graphemes
        {
            let (row, cell) = rac(&page, 0, 0);
            *cell = Cell::init(0x09);
            page.append_grapheme(row, cell, 0x0A).unwrap();
            page.append_grapheme(row, cell, 0x0B).unwrap();
        }

        // Clone it
        let page2 = TestPage(page.clone().unwrap());
        {
            let (row, cell) = rac(&page2, 0, 0);
            assert!(row.grapheme());
            assert!(cell.has_grapheme());
            assert_eq!(&[0x0A, 0x0B][..], page2.lookup_grapheme(cell).unwrap());
        }
    }

    #[test]
    fn page_clone_styles() {
        let mut page = init(cap3(10, 10, 8));

        // Write with some styles
        {
            let mem = page.memory;
            let id = unsafe { page.styles.add(mem, bold()) }.unwrap();

            for x in 0..page.size.cols as usize {
                let (row, cell) = rac(&page, x, 0);
                row.set_styled(true);
                let mut c = cp_cell(x as u32 + 1);
                c.set_style_id(id);
                *cell = c;
                unsafe { page.styles.use_(page.memory, id) };
            }
        }

        // Clone it
        let page2 = TestPage(page.clone().unwrap());
        {
            let id: u16 = {
                let (_, cell) = rac(&page2, 0, 0);
                cell.style_id()
            };

            for x in 0..page.size.cols as usize {
                let (row, cell) = rac(&page, x, 0);
                assert!(row.styled());
                assert_eq!(id, cell.style_id());
            }

            let style = unsafe { *page.styles.get(page.memory, id) };
            assert!(bold().eql(&style));
        }
    }

    #[test]
    fn page_clone_from() {
        let page = init(cap3(10, 10, 8));

        // Write
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            *cell = cp_cell(y as u32);
        }

        // Clone
        let mut page2 = init(cap3(10, 10, 8));
        unsafe { page2.clone_from(&*page, 0, page.size.rows as usize) }.unwrap();

        // Read it again
        for y in 0..page2.capacity.rows as usize {
            let (_, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32, cell.content_codepoint());
        }

        // Write again
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            *cell = cp_cell(0);
        }

        // Read it again, should be unchanged
        for y in 0..page2.capacity.rows as usize {
            let (_, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32, cell.content_codepoint());
        }

        // Read the original
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            assert_eq!(0, cell.content_codepoint());
        }
    }

    #[test]
    fn page_clone_from_shrink_columns() {
        let page = init(cap3(10, 10, 8));

        // Write
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            *cell = cp_cell(y as u32);
        }

        // Clone
        let mut page2 = init(cap3(5, 10, 8));
        unsafe { page2.clone_from(&*page, 0, page.size.rows as usize) }.unwrap();
        assert_eq!(5 as size::CellCountInt, page2.size.cols);

        // Read it again
        for y in 0..page2.capacity.rows as usize {
            let (_, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32, cell.content_codepoint());
        }
    }

    #[test]
    fn page_clone_from_partial() {
        let page = init(cap3(10, 10, 8));

        // Write
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            *cell = cp_cell(y as u32);
        }

        // Clone
        let mut page2 = init(cap3(10, 10, 8));
        unsafe { page2.clone_from(&*page, 0, 5) }.unwrap();

        // Read it again
        for y in 0..5 {
            let (_, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32, cell.content_codepoint());
        }
        for y in 5..page2.size.rows as usize {
            let (_, cell) = rac(&page2, 1, y);
            assert_eq!(0, cell.content_codepoint());
        }
    }

    #[test]
    fn page_clone_from_hyperlinks_exact_capacity() {
        let mut page = init(Capacity::new(50, 50));

        // Ensure our page can accommodate the capacity.
        let hyperlink_cap = page.hyperlink_capacity();
        assert!(hyperlink_cap <= page.size.cols as usize * page.size.rows as usize);

        // Create a hyperlink.
        let hyperlink_id = page
            .insert_hyperlink(&link(HyperlinkId::Implicit(0), b"https://example.com"))
            .unwrap();

        // Fill the exact cap with cells.
        'fill: for x in 0..page.size.cols as usize {
            for y in 0..page.size.rows as usize {
                let (row, cell) = rac(&page, x, y);
                *cell = cp_cell(42);
                page.set_hyperlink(row, cell, hyperlink_id).unwrap();
                unsafe { page.hyperlink_set.use_(page.memory, hyperlink_id) };

                if page.hyperlink_count() == hyperlink_cap {
                    break 'fill;
                }
            }
        }
        assert_eq!(page.hyperlink_count(), page.hyperlink_capacity());

        // Clone the full page
        let mut page2 = init(page.capacity);
        unsafe { page2.clone_from(&*page, 0, page.size.rows as usize) }.unwrap();

        // We should have the same number of hyperlinks
        assert_eq!(page2.hyperlink_count(), page.hyperlink_count());
    }

    #[test]
    fn page_clone_from_graphemes() {
        let mut page = init(cap3(10, 10, 8));

        // Write
        for y in 0..page.capacity.rows as usize {
            let (row, cell) = rac(&page, 1, y);
            *cell = cp_cell(y as u32 + 1);
            page.append_grapheme(row, cell, 0x0A).unwrap();
        }

        // Clone
        let mut page2 = init(cap3(10, 10, 8));
        unsafe { page2.clone_from(&*page, 0, page.size.rows as usize) }.unwrap();

        // Read it again
        for y in 0..page2.capacity.rows as usize {
            let (row, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32 + 1, cell.content_codepoint());
            assert!(row.grapheme());
            assert!(cell.has_grapheme());
            assert_eq!(&[0x0A][..], page2.lookup_grapheme(cell).unwrap());
        }

        // Write again
        for y in 0..page.capacity.rows as usize {
            let (row, cell) = rac(&page, 1, y);
            page.clear_grapheme(cell);
            page.update_row_grapheme_flag(row);
            *cell = cp_cell(0);
        }

        // Read it again, should be unchanged
        for y in 0..page2.capacity.rows as usize {
            let (row, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32 + 1, cell.content_codepoint());
            assert!(row.grapheme());
            assert!(cell.has_grapheme());
            assert_eq!(&[0x0A][..], page2.lookup_grapheme(cell).unwrap());
        }

        // Read the original
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            assert_eq!(0, cell.content_codepoint());
        }
    }

    #[test]
    fn page_clone_from_frees_dst_graphemes() {
        let page = init(cap3(10, 10, 8));
        for y in 0..page.capacity.rows as usize {
            let (_, cell) = rac(&page, 1, y);
            *cell = cp_cell(y as u32 + 1);
        }

        // Clone
        let mut page2 = init(cap3(10, 10, 8));
        for y in 0..page2.capacity.rows as usize {
            let (row, cell) = rac(&page2, 1, y);
            *cell = cp_cell(y as u32 + 1);
            page2.append_grapheme(row, cell, 0x0A).unwrap();
        }

        // Clone from page which has no graphemes.
        unsafe { page2.clone_from(&*page, 0, page.size.rows as usize) }.unwrap();

        // Read it again
        for y in 0..page2.capacity.rows as usize {
            let (row, cell) = rac(&page2, 1, y);
            assert_eq!(y as u32 + 1, cell.content_codepoint());
            assert!(!row.grapheme());
            assert!(!cell.has_grapheme());
        }
        assert_eq!(0, page2.grapheme_count());
    }

    #[test]
    fn page_clone_row_from_partial() {
        let page = init(cap3(10, 10, 8));

        // Write
        {
            let y = 0;
            for x in 0..page.size.cols as usize {
                let (_, cell) = rac(&page, x, y);
                *cell = cp_cell(x as u32 + 1);
            }
        }

        // Clone
        let mut page2 = init(cap3(10, 10, 8));
        let dst_row = row(&page2, 0);
        unsafe { page2.clone_partial_row_from(&*page, dst_row, row(&page, 0), 2, 8) }.unwrap();

        // Read it again
        {
            let y = 0;
            for x in 0..page2.size.cols as usize {
                let expected: u32 = if (2..8).contains(&x) { x as u32 + 1 } else { 0 };
                let (_, cell) = rac(&page2, x, y);
                assert_eq!(expected, cell.content_codepoint());
            }
        }
    }

    #[test]
    fn page_clone_row_from_partial_grapheme_in_non_copied_source_region() {
        let mut page = init(cap3(10, 10, 8));

        // Write
        {
            let y = 0;
            for x in 0..page.size.cols as usize {
                let (_, cell) = rac(&page, x, y);
                *cell = cp_cell(x as u32 + 1);
            }
            {
                let (row, cell) = rac(&page, 0, y);
                page.append_grapheme(row, cell, 0x0A).unwrap();
            }
            {
                let (row, cell) = rac(&page, 9, y);
                page.append_grapheme(row, cell, 0x0A).unwrap();
            }
        }
        assert_eq!(2, page.grapheme_count());

        // Clone
        let mut page2 = init(cap3(10, 10, 8));
        let dst_row = row(&page2, 0);
        unsafe { page2.clone_partial_row_from(&*page, dst_row, row(&page, 0), 2, 8) }.unwrap();

        // Read it again
        {
            let y = 0;
            for x in 0..page2.size.cols as usize {
                let expected: u32 = if (2..8).contains(&x) { x as u32 + 1 } else { 0 };
                let (_, cell) = rac(&page2, x, y);
                assert_eq!(expected, cell.content_codepoint());
                assert!(!cell.has_grapheme());
            }
            {
                let (row, _) = rac(&page2, 9, y);
                assert!(!row.grapheme());
            }
        }
        assert_eq!(0, page2.grapheme_count());
    }

    #[test]
    fn page_clone_row_from_partial_grapheme_in_non_copied_dest_region() {
        let page = init(cap3(10, 10, 8));

        // Write
        {
            let y = 0;
            for x in 0..page.size.cols as usize {
                let (_, cell) = rac(&page, x, y);
                *cell = cp_cell(x as u32 + 1);
            }
        }
        assert_eq!(0, page.grapheme_count());

        // Clone
        let mut page2 = init(cap3(10, 10, 8));
        {
            let y = 0;
            for x in 0..page2.size.cols as usize {
                let (_, cell) = rac(&page2, x, y);
                *cell = cp_cell(0xBB);
            }
            {
                let (row, cell) = rac(&page2, 0, y);
                page2.append_grapheme(row, cell, 0x0A).unwrap();
            }
            {
                let (row, cell) = rac(&page2, 9, y);
                page2.append_grapheme(row, cell, 0x0A).unwrap();
            }
        }
        let dst_row = row(&page2, 0);
        unsafe { page2.clone_partial_row_from(&*page, dst_row, row(&page, 0), 2, 8) }.unwrap();

        // Read it again
        {
            let y = 0;
            for x in 0..page2.size.cols as usize {
                let expected: u32 = if (2..8).contains(&x) { x as u32 + 1 } else { 0xBB };
                let (_, cell) = rac(&page2, x, y);
                assert_eq!(expected, cell.content_codepoint());
            }
            {
                let (row, _) = rac(&page2, 9, y);
                assert!(row.grapheme());
            }
        }
        assert_eq!(2, page2.grapheme_count());
    }

    fn add_empty_hyperlink(page: &mut Page) -> hyperlink::Id {
        let ctx = hyperlink::SetContext::new(page as *mut Page);
        unsafe {
            (*ctx.page)
                .hyperlink_set
                .add_context(
                    (*ctx.page).memory,
                    hyperlink::PageEntry {
                        id: hyperlink::PageEntryId::Implicit(0),
                        uri: OffsetSlice::default(),
                    },
                    &ctx,
                )
                .unwrap()
        }
    }

    #[test]
    fn page_clone_row_from_partial_hyperlink_in_same_page_copy() {
        let mut page = init(Capacity::new(10, 10));

        // We need to create a hyperlink.
        let hyperlink_id = add_empty_hyperlink(&mut page);

        // Write
        {
            let y = 0;
            for x in 0..page.size.cols as usize {
                let (_, cell) = rac(&page, x, y);
                *cell = cp_cell(x as u32 + 1);
            }

            // Hyperlink in a single cell
            {
                let (row, cell) = rac(&page, 7, y);
                page.set_hyperlink(row, cell, hyperlink_id).unwrap();
            }
        }
        assert_eq!(1, page.hyperlink_count());

        // Clone into the same page
        {
            let p: *mut Page = &mut *page;
            unsafe { (*p).clone_partial_row_from(p, row(&*p, 1), row(&*p, 0), 2, 8) }.unwrap();
        }

        // Read it again
        {
            let y = 1;
            for x in 0..page.size.cols as usize {
                let expected: u32 = if (2..8).contains(&x) { x as u32 + 1 } else { 0 };
                let (_, cell) = rac(&page, x, y);
                assert_eq!(expected, cell.content_codepoint());
            }
            {
                let (row, cell) = rac(&page, 7, y);
                assert!(row.hyperlink());
                assert!(cell.hyperlink());
            }
        }
        assert_eq!(2, page.hyperlink_count());
    }

    #[test]
    fn page_clone_row_from_partial_hyperlink_in_same_page_omit() {
        let mut page = init(Capacity::new(10, 10));

        // We need to create a hyperlink.
        let hyperlink_id = add_empty_hyperlink(&mut page);

        // Write
        {
            let y = 0;
            for x in 0..page.size.cols as usize {
                let (_, cell) = rac(&page, x, y);
                *cell = cp_cell(x as u32 + 1);
            }

            // Hyperlink in a single cell
            {
                let (row, cell) = rac(&page, 7, y);
                page.set_hyperlink(row, cell, hyperlink_id).unwrap();
            }
        }
        assert_eq!(1, page.hyperlink_count());

        // Clone into the same page
        {
            let p: *mut Page = &mut *page;
            unsafe { (*p).clone_partial_row_from(p, row(&*p, 1), row(&*p, 0), 2, 6) }.unwrap();
        }

        // Read it again
        {
            let y = 1;
            for x in 0..page.size.cols as usize {
                let expected: u32 = if (2..6).contains(&x) { x as u32 + 1 } else { 0 };
                let (_, cell) = rac(&page, x, y);
                assert_eq!(expected, cell.content_codepoint());
            }
            {
                let (row, cell) = rac(&page, 7, y);
                assert!(!row.hyperlink());
                assert!(!cell.hyperlink());
            }
        }
        assert_eq!(1, page.hyperlink_count());
    }

    #[test]
    fn page_move_cells_text_only() {
        let mut page = init(cap3(10, 10, 8));

        // Write
        for x in 0..page.capacity.cols as usize {
            let (_, cell) = rac(&page, x, 0);
            *cell = cp_cell(x as u32 + 1);
        }

        let src = row(&page, 0);
        let dst = row(&page, 1);
        let cols = page.capacity.cols as usize;
        page.move_cells(src, 0, dst, 0, cols);

        // New rows should have text
        for x in 0..page.capacity.cols as usize {
            let (_, cell) = rac(&page, x, 1);
            assert_eq!(x as u32 + 1, cell.content_codepoint());
        }

        // Old row should be blank
        for x in 0..page.capacity.cols as usize {
            let (_, cell) = rac(&page, x, 0);
            assert_eq!(0, cell.content_codepoint());
        }
    }

    #[test]
    fn page_move_cells_graphemes() {
        let mut page = init(cap3(10, 10, 8));

        // Write
        for x in 0..page.size.cols as usize {
            let (row, cell) = rac(&page, x, 0);
            *cell = cp_cell(x as u32 + 1);
            page.append_grapheme(row, cell, 0x0A).unwrap();
        }
        let original_count = page.grapheme_count();

        let src = row(&page, 0);
        let dst = row(&page, 1);
        let cols = page.size.cols as usize;
        page.move_cells(src, 0, dst, 0, cols);
        assert_eq!(original_count, page.grapheme_count());

        // New rows should have text
        for x in 0..page.size.cols as usize {
            let (_, cell) = rac(&page, x, 1);
            assert_eq!(x as u32 + 1, cell.content_codepoint());
            assert_eq!(&[0x0A][..], page.lookup_grapheme(cell).unwrap());
        }

        // Old row should be blank
        for x in 0..page.size.cols as usize {
            let (_, cell) = rac(&page, x, 0);
            assert_eq!(0, cell.content_codepoint());
        }
    }

    #[test]
    fn page_verify_integrity_graphemes_good() {
        let mut page = init(cap3(10, 10, 8));

        // Write
        for x in 0..page.size.cols as usize {
            let (row, cell) = rac(&page, x, 0);
            *cell = cp_cell(x as u32 + 1);
            page.append_grapheme(row, cell, 0x0A).unwrap();
        }

        page.verify_integrity().unwrap();
    }

    #[test]
    fn page_verify_integrity_grapheme_row_not_marked() {
        let mut page = init(cap3(10, 10, 8));

        // Write
        for x in 0..page.size.cols as usize {
            let (row, cell) = rac(&page, x, 0);
            *cell = cp_cell(x as u32 + 1);
            page.append_grapheme(row, cell, 0x0A).unwrap();
        }

        // Make invalid by unmarking the row
        row(&page, 0).set_grapheme(false);

        assert_eq!(Err(IntegrityError::UnmarkedGraphemeRow), page.verify_integrity());
    }

    fn write_styled_row(page: &mut Page, id: StyleId) {
        for x in 0..page.size.cols as usize {
            let (row, cell) = rac(page, x, 0);
            row.set_styled(true);
            let mut c = cp_cell(x as u32 + 1);
            c.set_style_id(id);
            *cell = c;
            unsafe { page.styles.use_(page.memory, id) };
        }
    }

    #[test]
    fn page_verify_integrity_styles_good() {
        let mut page = init(cap3(10, 10, 8));

        // Upsert a style we'll use
        let mem = page.memory;
        let id = unsafe { page.styles.add(mem, bold()) }.unwrap();

        // Write
        write_styled_row(&mut page, id);

        // The original style add would have incremented the
        // ref count too, so release it to balance that out.
        unsafe { page.styles.release(mem, id) };

        page.verify_integrity().unwrap();
    }

    #[test]
    fn page_verify_integrity_styles_ref_count_mismatch() {
        let mut page = init(cap3(10, 10, 8));

        // Upsert a style we'll use
        let mem = page.memory;
        let id = unsafe { page.styles.add(mem, bold()) }.unwrap();

        // Write
        write_styled_row(&mut page, id);

        // The original style add would have incremented the
        // ref count too, so release it to balance that out.
        unsafe { page.styles.release(mem, id) };

        // Miss a ref
        unsafe { page.styles.release(mem, id) };

        assert_eq!(Err(IntegrityError::MismatchedStyleRef), page.verify_integrity());
    }

    #[test]
    fn page_verify_integrity_zero_rows() {
        let mut page = init(cap3(10, 10, 8));
        page.size.rows = 0;
        assert_eq!(Err(IntegrityError::ZeroRowCount), page.verify_integrity());
    }

    #[test]
    fn page_verify_integrity_zero_cols() {
        let mut page = init(cap3(10, 10, 8));
        page.size.cols = 0;
        assert_eq!(Err(IntegrityError::ZeroColCount), page.verify_integrity());
    }

    fn item_size() -> usize {
        core::mem::size_of::<hyperlink::SetItem>()
    }

    #[test]
    fn page_exact_row_capacity_empty_rows() {
        let page = init(Capacity {
            styles: 8,
            hyperlink_bytes: (32 * item_size()) as u16,
            string_bytes: 512,
            ..Capacity::new(10, 10)
        });

        // Empty page: all capacity fields should be 0 (except cols/rows)
        let cap = page.exact_row_capacity(0, 5);
        assert_eq!(10, cap.cols);
        assert_eq!(5, cap.rows);
        assert_eq!(0, cap.styles);
        assert_eq!(0, cap.grapheme_bytes);
        assert_eq!(0, cap.hyperlink_bytes);
        assert_eq!(0, cap.string_bytes);
    }

    fn clone_rows_exact(page: &Page, rows: usize) {
        let cap = page.exact_row_capacity(0, rows);
        let mut cloned = init(cap);
        for y in 0..rows {
            let src_row = row(page, y);
            let dst_row = row(&cloned, y);
            unsafe { cloned.clone_row_from(page, dst_row, src_row) }.unwrap();
        }
        let cloned_cap = cloned.exact_row_capacity(0, rows);
        assert_eq!(cap, cloned_cap);
    }

    #[test]
    fn page_exact_row_capacity_styles() {
        let mut page = init(cap3(10, 10, 8));
        let mem = page.memory;

        // No styles: capacity should be 0
        {
            let cap = page.exact_row_capacity(0, 5);
            assert_eq!(0, cap.styles);
        }

        // Add one style to a cell
        let style1_id = unsafe { page.styles.add(mem, bold()) }.unwrap();
        {
            let (row, cell) = rac(&page, 0, 0);
            row.set_styled(true);
            cell.set_style_id(style1_id);
        }

        // One unique style - capacity accounts for load factor
        let cap_one_style = page.exact_row_capacity(0, 5);
        {
            assert_eq!(StyleSet::capacity_for_count(1), cap_one_style.styles as usize);
        }

        // Add same style to another cell (duplicate) - capacity unchanged
        {
            let (_, cell) = rac(&page, 1, 0);
            cell.set_style_id(style1_id);
        }
        {
            let cap = page.exact_row_capacity(0, 5);
            assert_eq!(cap_one_style.styles, cap.styles);
        }

        // Add a different style
        let mut italic = Style::default();
        italic.flags.italic = true;
        let style2_id = unsafe { page.styles.add(mem, italic) }.unwrap();
        {
            let (_, cell) = rac(&page, 2, 0);
            cell.set_style_id(style2_id);
        }

        // Two unique styles - capacity accounts for load factor
        let cap_two_styles = page.exact_row_capacity(0, 5);
        {
            assert_eq!(StyleSet::capacity_for_count(2), cap_two_styles.styles as usize);
            assert!(cap_two_styles.styles > cap_one_style.styles);
        }

        // Style outside the row range should not be counted
        {
            let (row, cell) = rac(&page, 0, 7);
            row.set_styled(true);
            let mut underline = Style::default();
            underline.flags.underline = crate::terminal::sgr::Underline::Single;
            cell.set_style_id(unsafe { page.styles.add(mem, underline) }.unwrap());
        }
        {
            let cap = page.exact_row_capacity(0, 5);
            assert_eq!(cap_two_styles.styles, cap.styles);
        }

        // Full range includes the new style
        {
            let cap = page.exact_row_capacity(0, 10);
            assert_eq!(StyleSet::capacity_for_count(3), cap.styles as usize);
        }

        // Verify clone works with exact capacity and produces same result
        clone_rows_exact(&page, 5);
    }

    #[test]
    fn page_exact_row_capacity_single_style_clone() {
        // Regression test: verify a single style can be cloned with exact capacity.
        // This tests that capacityForCount properly accounts for ID 0 being reserved.
        let mut page = init(cap3(10, 2, 8));
        let mem = page.memory;

        // Add exactly one style to row 0
        let style_id = unsafe { page.styles.add(mem, bold()) }.unwrap();
        {
            let (row, cell) = rac(&page, 0, 0);
            row.set_styled(true);
            cell.set_style_id(style_id);
        }

        // exactRowCapacity for just row 0 should give capacity for 1 style
        let cap = page.exact_row_capacity(0, 1);
        assert_eq!(StyleSet::capacity_for_count(1), cap.styles as usize);

        // Create a new page with exact capacity and clone
        let mut cloned = init(cap);

        let src_row = row(&page, 0);
        let dst_row = row(&cloned, 0);

        // This must not fail with StyleSetOutOfMemory
        unsafe { cloned.clone_row_from(&*page, dst_row, src_row) }.unwrap();

        // Verify the style was cloned correctly
        let (_, cloned_cell) = rac(&cloned, 0, 0);
        assert!(cloned_cell.style_id() != style::DEFAULT_ID);
    }

    #[test]
    fn page_exact_row_capacity_styles_max_single_row() {
        let mut page = init(Capacity {
            styles: size::StyleCountInt::MAX,
            ..Capacity::new(size::CellCountInt::MAX, 1)
        });
        let mem = page.memory;

        // Style our first row
        let first_row = row(&page, 0);
        first_row.set_styled(true);

        // Fill cells with styles until we get OOM, but limit to a reasonable count
        // to avoid overflow when computing capacityForCount near maxInt
        let cells = unsafe { &mut *page.get_cells(first_row) };
        let mut count: usize = 0;
        let max_count: usize = 1000; // Limit to avoid overflow in capacity calculation
        for (i, cell) in cells.iter_mut().enumerate() {
            if count >= max_count {
                break;
            }
            let style = Style {
                fg_color: style::Color::Rgb(color::Rgb::new((i & 0xFF) as u8, ((i >> 8) & 0xFF) as u8, 0)),
                ..Default::default()
            };
            let Ok(style_id) = (unsafe { page.styles.add(mem, style) }) else {
                break;
            };
            cell.set_style_id(style_id);
            count += 1;
        }

        // Verify we added a meaningful number of styles
        assert!(count > 0);

        // Capacity should be at least count (adjusted for load factor)
        let cap = page.exact_row_capacity(0, 1);
        assert_eq!(StyleSet::capacity_for_count(count), cap.styles as usize);
    }

    #[test]
    fn page_exact_row_capacity_grapheme_bytes() {
        let mut page = init(cap3(10, 10, 8));

        // No graphemes: capacity should be 0
        {
            let cap = page.exact_row_capacity(0, 5);
            assert_eq!(0, cap.grapheme_bytes);
        }

        // Add one grapheme (1 codepoint) to a cell - rounds up to grapheme_chunk
        {
            let (row, cell) = rac(&page, 0, 0);
            *cell = Cell::init('a' as u32);
            page.append_grapheme(row, cell, 0x0301).unwrap(); // combining acute accent
        }
        {
            let cap = page.exact_row_capacity(0, 5);
            // 1 codepoint = 4 bytes, rounds up to grapheme_chunk (16)
            assert_eq!(GRAPHEME_CHUNK, cap.grapheme_bytes as usize);
        }

        // Add another grapheme to a different cell - should sum
        {
            let (row, cell) = rac(&page, 1, 0);
            *cell = Cell::init('e' as u32);
            page.append_grapheme(row, cell, 0x0300).unwrap(); // combining grave accent
        }
        {
            let cap = page.exact_row_capacity(0, 5);
            // 2 graphemes, each 1 codepoint = 2 * grapheme_chunk
            assert_eq!(GRAPHEME_CHUNK * 2, cap.grapheme_bytes as usize);
        }

        // Add a larger grapheme (multiple codepoints) that fits in one chunk
        {
            let (row, cell) = rac(&page, 2, 0);
            *cell = Cell::init('o' as u32);
            page.append_grapheme(row, cell, 0x0301).unwrap();
            page.append_grapheme(row, cell, 0x0302).unwrap();
            page.append_grapheme(row, cell, 0x0303).unwrap();
        }
        {
            let cap = page.exact_row_capacity(0, 5);
            // First two cells: 2 * grapheme_chunk
            // Third cell: 3 codepoints = 12 bytes, rounds up to grapheme_chunk
            assert_eq!(GRAPHEME_CHUNK * 3, cap.grapheme_bytes as usize);
        }

        // Grapheme outside the row range should not be counted
        {
            let (row, cell) = rac(&page, 0, 7);
            *cell = Cell::init('x' as u32);
            page.append_grapheme(row, cell, 0x0304).unwrap();
        }
        {
            let cap = page.exact_row_capacity(0, 5);
            assert_eq!(GRAPHEME_CHUNK * 3, cap.grapheme_bytes as usize);
        }

        // Full range includes the new grapheme
        {
            let cap = page.exact_row_capacity(0, 10);
            assert_eq!(GRAPHEME_CHUNK * 4, cap.grapheme_bytes as usize);
        }

        // Verify clone works with exact capacity and produces same result
        clone_rows_exact(&page, 5);
    }

    #[test]
    fn page_exact_row_capacity_grapheme_bytes_larger_than_chunk() {
        let mut page = init(cap3(10, 10, 8));

        // Add a grapheme larger than one chunk (grapheme_chunk_len = 4 codepoints)
        let (row, cell) = rac(&page, 0, 0);
        *cell = Cell::init('a' as u32);

        // Add 6 codepoints - requires 2 chunks (6 * 4 = 24 bytes, rounds up to 32)
        for i in 0..6 {
            page.append_grapheme(row, cell, 0x0300 + i).unwrap();
        }

        let cap = page.exact_row_capacity(0, 1);
        // 6 codepoints = 24 bytes, alignForward(24, 16) = 32
        assert_eq!(32, cap.grapheme_bytes);

        // Verify clone works with exact capacity and produces same result
        clone_rows_exact(&page, 1);
    }

    #[test]
    fn page_exact_row_capacity_hyperlinks() {
        let mut page = init(Capacity {
            styles: 8,
            hyperlink_bytes: (32 * item_size()) as u16,
            string_bytes: 512,
            ..Capacity::new(10, 10)
        });

        // No hyperlinks: capacity should be 0
        {
            let cap = page.exact_row_capacity(0, 5);
            assert_eq!(0, cap.hyperlink_bytes);
            assert_eq!(0, cap.string_bytes);
        }

        // Add one hyperlink with implicit ID
        let uri1 = b"https://example.com";
        let id1 = {
            let (row, cell) = rac(&page, 0, 0);

            // Create and add hyperlink entry
            let id = page.insert_hyperlink(&link(HyperlinkId::Implicit(1), uri1)).unwrap();
            page.set_hyperlink(row, cell, id).unwrap();
            id
        };
        // 1 hyperlink - capacity accounts for load factor
        let cap_one_link = page.exact_row_capacity(0, 5);
        {
            assert_eq!(
                hyperlink::Set::capacity_for_count(1) * item_size(),
                cap_one_link.hyperlink_bytes as usize
            );
            // URI "https://example.com" = 19 bytes, rounds up to string_chunk (32)
            assert_eq!(STRING_CHUNK, cap_one_link.string_bytes as usize);
        }

        // Add same hyperlink to another cell (duplicate ID) - capacity unchanged
        {
            let (row, cell) = rac(&page, 1, 0);

            // Use the same hyperlink ID for another cell
            unsafe { page.hyperlink_set.use_(page.memory, id1) };
            page.set_hyperlink(row, cell, id1).unwrap();
        }
        {
            let cap = page.exact_row_capacity(0, 5);
            assert_eq!(cap_one_link.hyperlink_bytes, cap.hyperlink_bytes);
            assert_eq!(cap_one_link.string_bytes, cap.string_bytes);
        }

        // Add a different hyperlink with explicit ID
        let uri2 = b"https://other.example.org/path";
        let explicit_id = b"my-link-id";
        {
            let (row, cell) = rac(&page, 2, 0);

            let id = page
                .insert_hyperlink(&link(HyperlinkId::Explicit(Cow::Borrowed(explicit_id)), uri2))
                .unwrap();
            page.set_hyperlink(row, cell, id).unwrap();
        }
        // 2 hyperlinks - capacity accounts for load factor
        let cap_two_links = page.exact_row_capacity(0, 5);
        {
            assert_eq!(
                hyperlink::Set::capacity_for_count(2) * item_size(),
                cap_two_links.hyperlink_bytes as usize
            );
            // First URI: 19 bytes -> 32, Second URI: 30 bytes -> 32, Explicit ID: 10 bytes -> 32
            assert_eq!(STRING_CHUNK * 3, cap_two_links.string_bytes as usize);
        }

        // Hyperlink outside the row range should not be counted
        {
            let (row, cell) = rac(&page, 0, 7); // row 7 is outside range [0, 5)

            let id = page
                .insert_hyperlink(&link(HyperlinkId::Implicit(99), b"https://outside.example.com"))
                .unwrap();
            page.set_hyperlink(row, cell, id).unwrap();
        }
        {
            let cap = page.exact_row_capacity(0, 5);
            assert_eq!(cap_two_links.hyperlink_bytes, cap.hyperlink_bytes);
            assert_eq!(cap_two_links.string_bytes, cap.string_bytes);
        }

        // Full range includes the new hyperlink
        {
            let cap = page.exact_row_capacity(0, 10);
            assert_eq!(hyperlink::Set::capacity_for_count(3) * item_size(), cap.hyperlink_bytes as usize);
            // Third URI: 27 bytes -> 32
            assert_eq!(STRING_CHUNK * 4, cap.string_bytes as usize);
        }

        // Verify clone works with exact capacity and produces same result
        clone_rows_exact(&page, 5);
    }

    #[test]
    fn page_exact_row_capacity_single_hyperlink_clone() {
        // Regression test: verify a single hyperlink can be cloned with exact capacity.
        // This tests that capacityForCount properly accounts for ID 0 being reserved.
        let mut page = init(Capacity {
            styles: 8,
            hyperlink_bytes: (32 * item_size()) as u16,
            string_bytes: 512,
            ..Capacity::new(10, 2)
        });

        // Add exactly one hyperlink to row 0
        let uri = b"https://example.com";
        {
            let (row, cell) = rac(&page, 0, 0);
            let link_id = page.insert_hyperlink(&link(HyperlinkId::Implicit(1), uri)).unwrap();
            page.set_hyperlink(row, cell, link_id).unwrap();
        }

        // exactRowCapacity for just row 0 should give capacity for 1 hyperlink
        let cap = page.exact_row_capacity(0, 1);
        assert_eq!(hyperlink::Set::capacity_for_count(1) * item_size(), cap.hyperlink_bytes as usize);

        // Create a new page with exact capacity and clone
        let mut cloned = init(cap);

        let src_row = row(&page, 0);
        let dst_row = row(&cloned, 0);

        // This must not fail with HyperlinkSetOutOfMemory
        unsafe { cloned.clone_row_from(&*page, dst_row, src_row) }.unwrap();

        // Verify the hyperlink was cloned correctly
        let (_, cloned_cell) = rac(&cloned, 0, 0);
        assert!(cloned_cell.hyperlink());
    }

    #[test]
    fn page_exact_row_capacity_hyperlink_map_capacity_for_many_cells() {
        // A single hyperlink spanning many cells requires hyperlink_map capacity
        // based on cell count, not unique hyperlink count.
        let cols: usize = 50;
        let mut page = init(Capacity {
            styles: 8,
            hyperlink_bytes: (32 * item_size()) as u16,
            string_bytes: 512,
            ..Capacity::new(cols as u16, 2)
        });

        // Add one hyperlink spanning all 50 columns in row 0
        let uri = b"https://example.com";
        let id = {
            let (row, cell) = rac(&page, 0, 0);
            let link_id = page.insert_hyperlink(&link(HyperlinkId::Implicit(1), uri)).unwrap();
            page.set_hyperlink(row, cell, link_id).unwrap();
            link_id
        };

        // Apply same hyperlink to remaining cells in row 0
        for x in 1..cols {
            let (row, cell) = rac(&page, x, 0);
            unsafe { page.hyperlink_set.use_(page.memory, id) };
            page.set_hyperlink(row, cell, id).unwrap();
        }

        // exactRowCapacity must account for 50 hyperlink cells, not just 1 unique hyperlink
        let cap = page.exact_row_capacity(0, 1);

        // The hyperlink_bytes must be large enough that layout() computes sufficient
        // hyperlink_map capacity. With hyperlink_cell_multiplier=16, we need at least
        // ceil(50/16) = 4 hyperlink entries worth of bytes for the map.
        let min_for_map = cols.div_ceil(HYPERLINK_CELL_MULTIPLIER);
        let min_hyperlink_bytes = min_for_map * item_size();
        assert!(cap.hyperlink_bytes as usize >= min_hyperlink_bytes);

        // Create a new page with exact capacity and clone - must not fail
        let mut cloned = init(cap);

        let src_row = row(&page, 0);
        let dst_row = row(&cloned, 0);

        // This must not fail with HyperlinkMapOutOfMemory
        unsafe { cloned.clone_row_from(&*page, dst_row, src_row) }.unwrap();

        // Verify all hyperlinks were cloned correctly
        for x in 0..cols {
            let (_, cloned_cell) = rac(&cloned, x, 0);
            assert!(cloned_cell.hyperlink());
        }
    }

    #[test]
    fn page_layout_avoids_double_rounding_hyperlink_map_capacity() {
        let hyperlink_count = 3;
        let layout = Page::layout(Capacity {
            hyperlink_bytes: (hyperlink_count * item_size()) as u16,
            ..Capacity::new(1, 1)
        });

        // Three set entries request 48 usable map entries. Scaling that for the
        // 80% load factor needs 60 raw slots, which rounds once to 64. Rounding
        // the request before applying the load factor would allocate 128 slots.
        assert_eq!(64u32, layout.hyperlink_map_layout.capacity);
    }
}
