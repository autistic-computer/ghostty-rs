//! Port of `src/terminal/kitty/graphics_image.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - `std.Io`/allocator parameters are dropped.
//! - `Image` owns its pixel data (`Vec<u8>`) and animation (`Box`), so
//!   copying an image is a deep `clone`. The Zig code copies `Image` values
//!   freely because they only hold slices/pointers; Rust callers borrow
//!   stored images instead (see `ImageStorage::image_by_id`).
//! - `Image.renderData` returns [`DataRef`], a borrowed view of [`Data`].
//! - The PNG decoder takes no allocator in Rust; `LimitedAllocator` is
//!   emulated with [`sys::DecodeLimit`].
//! - The test-only `testing.tmpDir` is emulated with a directory under
//!   `target/tmp` (not under `/tmp`, matching Zig's `.zig-cache/tmp`, since
//!   files under `/tmp` are always accepted as temporary files).

/// Maximum file name length (`NAME_MAX`). The libc crate doesn't export it
/// for Apple targets; Darwin's `sys/syslimits.h` defines it as 255.
#[cfg(not(target_vendor = "apple"))]
const NAME_MAX: usize = libc::NAME_MAX as usize;
#[cfg(target_vendor = "apple")]
const NAME_MAX: usize = 255;

use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom};

use crate::pkg::wuffs;
use crate::terminal::kitty::graphics_animation as animation;
use crate::terminal::kitty::graphics_command as command;
use crate::terminal::page_list::Pin;
use crate::terminal::sys;

#[cfg(windows)]
use crate::terminal::kitty::windows as kitty_windows;

use self::command::{Compression, Format, Medium, Transmission};

const LOG_TARGET: &str = "kitty_gfx";

/// Maximum width or height of an image. Taken directly from Kitty.
const MAX_DIMENSION: u32 = 10000;

/// Maximum size in bytes, taken from Kitty.
const MAX_SIZE: usize = 400 * 1024 * 1024; // 400MB

/// `Image.Error` (plus `Allocator.Error`, which Zig unions into every
/// loading error set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InsufficientData,
    InvalidData,
    DecompressionFailed,
    DimensionsRequired,
    DimensionsTooLarge,
    FilePathTooLong,
    TemporaryFileNotInTempDir,
    TemporaryFileNotNamedCorrectly,
    UnsupportedFormat,
    UnsupportedMedium,
    UnsupportedDepth,
    OutOfMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// An image that is still being loaded. The image should be initialized
/// using init on the first chunk and then addData for each subsequent
/// chunk. Once all chunks have been added, complete should be called
/// to finalize the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadingImage {
    /// The in-progress image. The first chunk must have all the metadata
    /// so this comes from that initially.
    pub image: Image,

    /// The data that is being built up.
    pub data: Vec<u8>,

    /// This is non-null when a transmit and display command is given
    /// so that we display the image after it is fully loaded.
    pub display: Option<command::Display>,

    /// This is non-null when this load is an animation frame
    /// transmission (a=f) rather than a new image. On completion the
    /// data is composed into the target image's animation instead of
    /// being stored as an image.
    pub frame: Option<FrameContext>,

    /// Quiet is the quiet settings for the initial load command. This is
    /// used if q isn't set on subsequent chunks.
    pub quiet: command::Quiet,

    /// Response identifiers from the initial load command. Subsequent chunks
    /// omit these, so completion responses must use the saved values.
    pub response: command::Response,

    /// The temporary directory for file transmission (null means that
    /// temporary directory transmission is disabled).
    pub temporary_directory: Option<Vec<u8>>,
}

/// `LoadingImage.FrameContext`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameContext {
    /// The frame parameters from the initial a=f command. Chunked
    /// continuations only contribute payload bytes; all parameters
    /// come from the command that started the load, matching the
    /// protocol's requirement that chunks repeat a=f.
    pub cmd: command::AnimationFrameLoading,

    /// The generation of the target image when the load began.
    /// A different generation at completion means the image was
    /// replaced or evicted mid-transmission and the frame must be
    /// discarded rather than composed onto the wrong image.
    pub image_generation: u64,
}

/// The limits of the Kitty Graphics protocol we should allow
/// (Zig: `LoadingImage.Limits`).
///
/// This can be used to restrict the type of images and other
/// parameters for resource or security reasons. Note that depending
/// on how libghostty is compiled, some of these may be fully unsupported
/// and ignored (e.g. "file" on wasm32-freestanding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub file: bool,
    pub temporary_file: TemporaryFile,
    pub shared_memory: bool,
}

/// (Zig: the anonymous `Limits.temporary_file` union.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemporaryFile {
    Enabled {
        /// The directory to expect temporary files in.
        directory: Vec<u8>,
    },
    Disabled,
}

impl Limits {
    /// Enables all filesystem-related image transmission mediums. `path`
    /// is the temporary directory to expect files in when files are
    /// transmitted using said medium.
    pub fn all_with_temp_dir(path: &[u8]) -> Limits {
        Limits {
            file: true,
            temporary_file: TemporaryFile::Enabled {
                directory: path.to_vec(),
            },
            shared_memory: true,
        }
    }

    pub const DIRECT: Limits = Limits {
        file: false,
        temporary_file: TemporaryFile::Disabled,
        shared_memory: false,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DIRECT
    }
}

/// `LoadingImage.SharedMemoryRange`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SharedMemoryRange {
    start: usize,
    end: usize,
}

impl LoadingImage {
    /// A loading image with only the given image metadata and quiet
    /// setting (Zig: a `LoadingImage` struct literal with defaults).
    pub fn new(image: Image, quiet: command::Quiet) -> LoadingImage {
        LoadingImage {
            image,
            data: Vec::new(),
            display: None,
            frame: None,
            quiet,
            response: command::Response::default(),
            temporary_directory: None,
        }
    }

    /// Initialize a chunked immage from the first image transmission.
    /// If this is a multi-chunk image, this should only be the FIRST
    /// chunk.
    pub fn init(cmd: &command::Command, limits: &Limits) -> Result<LoadingImage, Error> {
        // Build our initial image from the properties sent via the control.
        // These can be overwritten by the data loading process. For example,
        // PNG loading sets the width/height from the data.
        let t = cmd.transmission().unwrap();

        // Validated here rather than while parsing so the response can
        // carry the image id, matching Kitty's initialize_load_data.
        if t.format_unknown {
            return Err(Error::UnsupportedFormat);
        }
        let mut result = LoadingImage {
            image: Image {
                id: t.image_id,
                number: t.image_number,
                width: t.width,
                height: t.height,
                compression: t.compression,
                format: t.format,
                metadata: Metadata {
                    transient: t.usage.transient,
                    ..Default::default()
                },
                ..Default::default()
            },

            data: Vec::new(),
            display: cmd.display(),
            frame: None,
            quiet: cmd.quiet,
            response: command::Response {
                id: t.image_id,
                image_number: t.image_number,
                placement_id: t.placement_id,
                ..Default::default()
            },
            temporary_directory: match &limits.temporary_file {
                TemporaryFile::Enabled { directory } => Some(directory.clone()),
                TemporaryFile::Disabled => None,
            },
        };

        // Special case for the direct medium, we just add the chunk directly.
        if t.medium == Medium::Direct {
            result.add_data(&cmd.data)?;
            return Ok(result);
        }

        // Verify our capabilities and limits allow this.
        {
            // Special case if we don't support decoding PNGs and the format
            // is a PNG we can save a lot of memory/effort buffering the
            // data but failing up front.
            if t.format == Format::Png && sys::decode_png().is_none() {
                return Err(Error::UnsupportedMedium);
            }

            // Verify the medium is allowed
            match t.medium {
                Medium::Direct => unreachable!(),
                Medium::File => {
                    if !limits.file {
                        return Err(Error::UnsupportedMedium);
                    }
                }
                Medium::TemporaryFile => {
                    if limits.temporary_file == TemporaryFile::Disabled {
                        return Err(Error::UnsupportedMedium);
                    }
                }
                Medium::SharedMemory => {
                    if !limits.shared_memory {
                        return Err(Error::UnsupportedMedium);
                    }
                }
            }
        }

        // Otherwise, the payload data is guaranteed to be a path.

        if !cfg!(windows) && memchr::memchr(0, &cmd.data).is_some() {
            // POSIX paths cannot contain internal nulls.
            log::warn!(target: LOG_TARGET, "invalid image path: BadPathName");
            return Err(Error::InvalidData);
        }

        // Depending on the medium, load the data from the path.
        match t.medium {
            Medium::Direct => unreachable!(), // handled above
            Medium::File => result.read_file(Medium::File, &t, &cmd.data)?,
            Medium::TemporaryFile => result.read_file(Medium::TemporaryFile, &t, &cmd.data)?,
            Medium::SharedMemory => result.read_shared_memory(&t, &cmd.data)?,
        }

        Ok(result)
    }

    /// Reads the data from a shared memory segment.
    #[cfg(any(target_os = "android", windows))]
    fn read_shared_memory(&mut self, _t: &Transmission, _path: &[u8]) -> Result<(), Error> {
        // android does not support POSIX shared memory.
        // windows is currently unsupported, does it support shm?
        Err(Error::UnsupportedMedium)
    }

    /// Reads the data from a shared memory segment.
    #[cfg(not(any(target_os = "android", windows)))]
    fn read_shared_memory(&mut self, t: &Transmission, path: &[u8]) -> Result<(), Error> {
        use std::ffi::CString;
        use std::os::fd::{FromRawFd, OwnedFd};

        // The Kitty protocol requires terminals to pass shared memory names
        // unmodified to shm_open, including names without a leading slash:
        // sandboxed macOS apps can only use names of the form
        // `<app group id>/<name>`. We only reject clearly malformed names.
        if !valid_shared_memory_name(path, NAME_MAX) {
            log::warn!(target: LOG_TARGET, "invalid shared memory name");
            return Err(Error::InvalidData);
        }

        let pathz = CString::new(path).map_err(|_| Error::InvalidData)?;

        // SAFETY: pathz is a valid NUL-terminated string.
        let fd = unsafe { libc::shm_open(pathz.as_ptr(), libc::O_RDONLY, 0) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            log::warn!(
                target: LOG_TARGET,
                "unable to open shared memory {}: {}",
                String::from_utf8_lossy(path),
                err
            );
            return Err(Error::InvalidData);
        }
        // SAFETY: shm_open returned a fresh, owned descriptor.
        let file = fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });

        // We unlink whatever name the client sent, as the Kitty protocol
        // requires. So a client can remove any shared memory object that
        // Ghostty can open, including another app group's on macOS if it
        // can guess the name. This applies to any name, not just ones
        // without a leading slash.
        struct Unlink(CString);
        impl Drop for Unlink {
            fn drop(&mut self) {
                // SAFETY: the name is a valid NUL-terminated string.
                unsafe { libc::shm_unlink(self.0.as_ptr()) };
            }
        }
        let _unlink = Unlink(pathz);

        // The size from stat on may be larger than our expected size because
        // shared memory has to be a multiple of the page size.
        let stat_size: usize = {
            let stat = file.metadata().map_err(|err| {
                log::warn!(
                    target: LOG_TARGET,
                    "unable to fstat shared memory {}: {}",
                    String::from_utf8_lossy(path),
                    err
                );
                Error::InvalidData
            })?;
            if stat.len() == 0 {
                return Err(Error::InvalidData);
            }
            usize::try_from(stat.len()).map_err(|_| Error::InvalidData)?
        };

        // Get the memory range we'll read. Validate it to make sure
        // it doesn't overflow.
        let range = self.shared_memory_range(t, stat_size)?;

        // SAFETY: mapping a valid fd read-only; the mapping is unmapped
        // below before returning.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                stat_size, // mmap always uses the stat size
                libc::PROT_READ,
                libc::MAP_SHARED,
                std::os::fd::AsRawFd::as_raw_fd(&file),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            let err = std::io::Error::last_os_error();
            log::warn!(
                target: LOG_TARGET,
                "unable to mmap shared memory {}: {}",
                String::from_utf8_lossy(path),
                err
            );
            return Err(Error::InvalidData);
        }

        // SAFETY: the mapping is stat_size bytes long and range is
        // validated to be within it.
        let bytes = unsafe { std::slice::from_raw_parts(map as *const u8, stat_size) };
        debug_assert!(self.data.is_empty());
        let res = self
            .data
            .try_reserve_exact(range.end - range.start)
            .map_err(|_| Error::OutOfMemory);
        if res.is_ok() {
            self.data.extend_from_slice(&bytes[range.start..range.end]);
        }
        // SAFETY: map/stat_size came from the successful mmap above.
        unsafe { libc::munmap(map, stat_size) };
        res
    }

    /// Returns the byte range to copy from a shared memory object.
    fn shared_memory_range(
        &self,
        t: &Transmission,
        stat_size: usize,
    ) -> Result<SharedMemoryRange, Error> {
        let expected_size: Option<usize> = match self.image.format {
            // PNG dimensions come from the decoded data.
            Format::Png => None,

            // Validate before multiplying because protocol dimensions are
            // u32 values and may otherwise overflow in safe builds.
            Format::Gray | Format::GrayAlpha | Format::Rgb | Format::Rgba => {
                if self.image.width > MAX_DIMENSION || self.image.height > MAX_DIMENSION {
                    return Err(Error::DimensionsTooLarge);
                }

                let bpp = Transmission::format_bpp(self.image.format) as usize;
                Some(self.image.width as usize * self.image.height as usize * bpp)
            }
        };

        // Get our start offset and validate its within the range of
        // the statted data.
        let start = usize::try_from(t.offset).map_err(|_| Error::InvalidData)?;
        if start > stat_size {
            return Err(Error::InvalidData);
        }

        // Validate that our length is within the stat range too.
        let available = stat_size - start;
        let data_size: usize = if t.size > 0 {
            usize::try_from(t.size).map_err(|_| Error::InvalidData)?
        } else if self.image.compression == Compression::None && expected_size.is_some() {
            expected_size.unwrap()
        } else {
            available
        };
        if data_size > MAX_SIZE || data_size > available {
            return Err(Error::InvalidData);
        }

        // data_size <= available guarantees this addition cannot overflow.
        Ok(SharedMemoryRange {
            start,
            end: start + data_size,
        })
    }

    /// Reads the data from a temporary file and returns it. This allocates
    /// and does not free any of the data, so the caller must free it.
    ///
    /// This will also delete the temporary file if it is in a safe location.
    fn read_file(&mut self, medium: Medium, t: &Transmission, path: &[u8]) -> Result<(), Error> {
        match medium {
            Medium::File | Medium::TemporaryFile => {}
            _ => unreachable!("readFile only supports file and temporary_file"),
        }

        // Some Windows paths are dangerous to even open, so the raw path
        // is checked before the open. The canonical path of the opened
        // file is checked again in validatedFilePath. See kitty_windows
        // for what is refused and why.
        #[cfg(windows)]
        if let Err(err) = kitty_windows::check_path(path) {
            log::warn!(target: LOG_TARGET, "invalid image path: {err}");
            return Err(Error::InvalidData);
        }

        // Open our file right away before we do validation. This avoids
        // TOCTOU issues.
        let mut file = match fs::File::open(path_from_bytes(path)) {
            Ok(f) => f,
            Err(err) => {
                log::warn!(target: LOG_TARGET, "failed to open image file: {err}");
                return Err(Error::InvalidData);
            }
        };

        // We'll populate a delete path if this is a temporary file. The
        // guard deletes it when this function returns, on success or error.
        struct DeleteGuard(Option<Vec<u8>>);
        impl Drop for DeleteGuard {
            fn drop(&mut self) {
                if let Some(p) = &self.0 {
                    if let Err(err) = fs::remove_file(path_from_bytes(p)) {
                        log::warn!(target: LOG_TARGET, "failed to delete temporary file: {err}");
                    }
                }
            }
        }
        let mut delete_path = DeleteGuard(None);

        // Derive the path from the open handle so the file we validate is the
        // exact file we read. Resolving a path before opening it would allow a
        // cooperating process to swap a symlink or directory entry in between.
        let abs_path = match Self::validated_file_path(&file) {
            Ok(p) => p,
            Err(err) => {
                log::warn!(target: LOG_TARGET, "failed to validate image file path: {err}");
                return Err(Error::InvalidData);
            }
        };

        // Temporary file logic
        if medium == Medium::TemporaryFile {
            let dir = self.temporary_directory.as_deref().unwrap();
            if !is_path_in_temp_dir(dir, &abs_path) {
                return Err(Error::TemporaryFileNotInTempDir);
            }
            if memchr::memmem::find(&abs_path, b"tty-graphics-protocol").is_none() {
                return Err(Error::TemporaryFileNotNamedCorrectly);
            }
            delete_path.0 = Some(abs_path.clone());
        }

        // File must be a regular file
        match file.metadata() {
            Ok(stat) => {
                if !stat.file_type().is_file() {
                    log::warn!(target: LOG_TARGET, "file is not a regular file kind={:?}", stat.file_type());
                    return Err(Error::InvalidData);
                }
            }
            Err(err) => {
                log::warn!(target: LOG_TARGET, "failed to stat file: {err}");
                return Err(Error::InvalidData);
            }
        }

        if t.offset > 0 {
            if let Err(err) = file.seek(SeekFrom::Start(t.offset as u64)) {
                log::warn!(target: LOG_TARGET, "failed to seek to offset {}: {}", t.offset, err);
                return Err(Error::InvalidData);
            }
        }

        // Read the file
        let mut managed: Vec<u8> = Vec::new();
        if t.size > 0 {
            // S is the exact number of bytes to read, not a maximum:
            // https://sw.kovidgoyal.net/kitty/graphics-protocol/#local-client
            let size = usize::try_from(t.size).map_err(|_| Error::InvalidData)?;
            if size > MAX_SIZE {
                return Err(Error::InvalidData);
            }

            // Read exact size
            managed
                .try_reserve_exact(size)
                .map_err(|_| Error::OutOfMemory)?;
            managed.resize(size, 0);
            if let Err(err) = file.read_exact(&mut managed) {
                log::warn!(target: LOG_TARGET, "failed to read image file: {err}");
                return Err(Error::InvalidData);
            }
        } else {
            let res = (&mut file)
                .take(MAX_SIZE as u64 + 1)
                .read_to_end(&mut managed);
            if res.is_err() || managed.len() > MAX_SIZE {
                log::warn!(target: LOG_TARGET, "failed to read image file: {:?}", res.err());
                return Err(Error::InvalidData);
            }
        }

        // Set our data
        debug_assert!(self.data.is_empty());
        self.data = managed;
        Ok(())
    }

    /// Returns the canonical path of an open file after applying the file
    /// transmission blocklist.
    fn validated_file_path(file: &fs::File) -> Result<Vec<u8>, Error> {
        let path = real_path(file).map_err(|_| Error::InvalidData)?;

        #[cfg(windows)]
        {
            kitty_windows::check_canonical_path(&path).map_err(|_| Error::InvalidData)?;
            return Ok(path);
        }

        // This is logic copied directly from Kitty, mostly. This is really
        // rough but it will catch obvious bad actors.
        #[allow(unreachable_code)]
        if path.starts_with(b"/proc/")
            || path.starts_with(b"/sys/")
            || (path.starts_with(b"/dev/") && !path.starts_with(b"/dev/shm/"))
        {
            return Err(Error::InvalidData);
        }

        Ok(path)
    }

    /// Adds a chunk of data to the image. Use this if the image
    /// is coming in chunks (the "m" parameter in the protocol).
    pub fn add_data(&mut self, data: &[u8]) -> Result<(), Error> {
        // If no data, skip
        if data.is_empty() {
            return Ok(());
        }

        // If our data would get too big, return an error
        if self.data.len() + data.len() > MAX_SIZE {
            log::warn!(target: LOG_TARGET, "image data too large max_size={MAX_SIZE}");
            return Err(Error::InvalidData);
        }

        // Ensure we have enough room to add the data
        // to the end of the ArrayList before doing so.
        self.data
            .try_reserve(data.len())
            .map_err(|_| Error::OutOfMemory)?;
        self.data.extend_from_slice(data);
        Ok(())
    }

    /// Complete the chunked image, returning a completed image.
    pub fn complete(&mut self) -> Result<Image, Error> {
        // Decompress the data if it is compressed.
        self.decompress()?;

        // Decode the png if we have to
        if self.image.format == Format::Png {
            self.decode_png()?;
        }

        let img = &self.image;

        // Validate our dimensions.
        if img.width == 0 || img.height == 0 {
            return Err(Error::DimensionsRequired);
        }
        if img.width > MAX_DIMENSION || img.height > MAX_DIMENSION {
            return Err(Error::DimensionsTooLarge);
        }

        // Data length must be what we expect.
        let bpp = Transmission::format_bpp(img.format) as usize;
        let expected_len = img.width as usize * img.height as usize * bpp;
        let actual_len = self.data.len();
        if self.frame.is_some() {
            // Kitty allows animation frames to exceed their expected length
            // and just truncates it. Not sure if thats expected but lets
            // allow it too.
            if actual_len < expected_len {
                log::warn!(
                    "insufficient frame data image id={} expected_len={} actual_len={}",
                    img.id,
                    expected_len,
                    actual_len
                );
                return Err(Error::InsufficientData);
            }
            self.data.truncate(expected_len);
        } else if actual_len != expected_len {
            log::warn!(
                "unexpected length image id={} width={} height={} bpp={} expected_len={} actual_len={}",
                img.id,
                img.width,
                img.height,
                bpp,
                expected_len,
                actual_len
            );
            return Err(Error::InvalidData);
        }

        // Everything looks good, copy the image data over.
        let mut result = std::mem::take(&mut self.image);
        result.data = Data::Complete(std::mem::take(&mut self.data));
        Ok(result)
    }

    /// Debug function to write the data to a file. This is useful for
    /// capturing some test data for unit tests.
    #[cfg(debug_assertions)]
    pub fn debug_dump(&self) -> std::io::Result<()> {
        let filename = format!(
            "image-{}-{}-{}x{}-{}.data",
            format_tag_name(self.image.format),
            match self.image.compression {
                Compression::None => "none",
                Compression::ZlibDeflate => "zlib_deflate",
            },
            self.image.width,
            self.image.height,
            self.image.id,
        );
        fs::write(filename, &self.data)
    }

    /// Decompress the data in-place.
    fn decompress(&mut self) -> Result<(), Error> {
        match self.image.compression {
            Compression::None => Ok(()),
            Compression::ZlibDeflate => self.decompress_zlib(),
        }
    }

    fn decompress_zlib(&mut self) -> Result<(), Error> {
        // Provide a bounded size hint based on image metadata for the initial allocation
        let size_hint: Option<usize> = (|| {
            if self.image.format == Format::Png
                || self.image.width == 0
                || self.image.height == 0
                || self.image.width > MAX_DIMENSION
                || self.image.height > MAX_DIMENSION
            {
                return None;
            }
            let pixels = (self.image.width as usize).checked_mul(self.image.height as usize)?;
            let bytes =
                pixels.checked_mul(Transmission::format_bpp(self.image.format) as usize)?;
            if bytes <= MAX_SIZE {
                Some(bytes)
            } else {
                None
            }
        })();
        let decompressed = match wuffs::zlib::decode(&self.data, MAX_SIZE, size_hint) {
            Ok(d) => d,
            Err(wuffs::Error::OutOfMemory) => return Err(Error::OutOfMemory),
            Err(wuffs::Error::WuffsError) | Err(wuffs::Error::Overflow) => {
                return Err(Error::DecompressionFailed)
            }
        };

        self.data = decompressed;

        // Make sure we note that our image is no longer compressed
        self.image.compression = Compression::None;
        Ok(())
    }

    /// Decode the data as PNG. This will also updated the image dimensions.
    fn decode_png(&mut self) -> Result<(), Error> {
        debug_assert!(self.image.format == Format::Png);

        let decode_png_fn = sys::decode_png().ok_or(Error::UnsupportedFormat)?;

        let limited = sys::DecodeLimit::init(MAX_SIZE);
        let result = match decode_png_fn(&self.data) {
            Ok(r) => r,
            Err(sys::DecodeError::InvalidData) => return Err(Error::InvalidData),
            Err(sys::DecodeError::OutOfMemory) => {
                return Err(if limited.limit_exceeded() {
                    Error::InvalidData
                } else {
                    Error::OutOfMemory
                })
            }
        };
        drop(limited);

        if result.data.len() > MAX_SIZE {
            log::warn!(
                target: LOG_TARGET,
                "png image too large size={} max_size={}",
                result.data.len(),
                MAX_SIZE
            );
            return Err(Error::InvalidData);
        }

        // Replace our data
        self.data = result.data;

        // Store updated image dimensions
        self.image.width = result.width;
        self.image.height = result.height;
        self.image.format = Format::Rgba;
        Ok(())
    }
}

/// The tag name of a format (Zig: `@tagName`).
#[cfg(debug_assertions)]
fn format_tag_name(format: Format) -> &'static str {
    match format {
        Format::Rgb => "rgb",
        Format::Rgba => "rgba",
        Format::Png => "png",
        Format::GrayAlpha => "gray_alpha",
        Format::Gray => "gray",
    }
}

/// The image's metadata (Zig: `packed struct(u32)`; only used in memory,
/// so a plain struct).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metadata {
    /// The image's transient usage hint, used to prioritize eviction.
    pub transient: bool,

    /// Set this if the image was loaded without an ID or number. Such
    /// images must not receive responses. Kitty gives these client ID
    /// 0 (unaddressable); our storage keys everything by one public
    /// u32 ID, so they get an ID from the upper half of the range
    /// that is guaranteed unused at assignment time, but a client
    /// that explicitly transmits that ID later can still replace
    /// them.
    pub implicit_id: bool,

    /// Number of placements referencing this image. (Zig: `u30`; see
    /// [`Metadata::MAX_PLACEMENT_COUNT`].)
    pub placement_count: u32,
}

impl Metadata {
    /// The maximum value of `placement_count` (`maxInt(u30)`).
    pub const MAX_PLACEMENT_COUNT: u32 = (1 << 30) - 1;
}

/// Image represents a single image whose metadata is fully known.
///
/// Complete image data is always fully decoded raw pixels: loading inflates
/// any zlib-compressed payload and decodes PNG into RGBA before an image is
/// completed, so `compression` is always `.none` and `format` is never `.png`
/// for a stored image. Pending image data reserves the exact decoded byte
/// length that will be attached later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub id: u32,
    pub number: u32,
    pub width: u32,
    pub height: u32,
    pub format: Format,
    pub compression: Compression,
    pub data: Data,
    pub metadata: Metadata,

    /// Unique, monotonically increasing stamp assigned each time an
    /// image is added to (or replaced in) an ImageStorage. A changed
    /// generation for a given image ID means the image contents may
    /// have changed, even if the dimensions and byte length are the
    /// same (e.g. a retransmission of the same ID). Stamps order by
    /// transmission time. Zero means "never stored".
    ///
    /// For animated images this also changes whenever the frame that
    /// should be displayed changes (advance, edit, or delete of the
    /// current frame), since consumers key texture caches off it.
    pub generation: u64,

    /// Identifies the transmission that created this image. It is set to
    /// the same stamp as `generation` when the image is added to (or
    /// replaced in) an ImageStorage, and then never changes. Zero means
    /// "never stored".
    ///
    /// Use `generation` to detect that the displayed pixels changed. Use
    /// `transmission` to detect that the image itself was replaced. The
    /// two differ for animations: showing another frame updates
    /// `generation` but leaves `transmission` alone.
    ///
    /// ```ignore
    /// let before = storage.image_by_id(1).unwrap().clone();
    ///
    /// // ... the client shows another animation frame ...
    ///
    /// let after = storage.image_by_id(1).unwrap();
    /// after.generation != before.generation; // true: new pixels shown
    /// after.transmission == before.transmission; // true: same image
    /// ```
    pub transmission: u64,

    /// Animation state, non-null once any animation command (a=f,
    /// a=a) has attached animation state to this image. Owned by the
    /// image; replaced/retransmitted images drop it, which implements
    /// the protocol's "retransmission resets the animation" rule.
    ///
    /// This is only ever attached to images stored in an ImageStorage
    /// and must only be mutated through the storage's own image.
    pub animation: Option<Box<animation::Animation>>,
}

impl Default for Image {
    fn default() -> Self {
        Image {
            id: 0,
            number: 0,
            width: 0,
            height: 0,
            format: Format::Rgb,
            compression: Compression::None,
            data: Data::default(),
            metadata: Metadata::default(),
            generation: 0,
            transmission: 0,
            animation: None,
        }
    }
}

/// `Image.Data`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    /// Owned, decoded image bytes. The empty default is not allocated.
    Complete(Vec<u8>),

    /// Expected decoded byte length for a payload that has not arrived.
    Pending(usize),
}

impl Default for Data {
    fn default() -> Self {
        Data::Complete(Vec::new())
    }
}

impl Data {
    /// Bytes reserved against the storage limit.
    pub fn len(&self) -> usize {
        match self {
            Data::Complete(data) => data.len(),
            Data::Pending(expected_len) => *expected_len,
        }
    }

    /// Returns decoded bytes when the payload is complete.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Data::Complete(data) => Some(data),
            Data::Pending(_) => None,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Data::Pending(_))
    }

    /// A borrowed view of this data.
    pub fn as_ref(&self) -> DataRef<'_> {
        match self {
            Data::Complete(data) => DataRef::Complete(data),
            Data::Pending(len) => DataRef::Pending(*len),
        }
    }
}

/// A borrowed [`Data`] (Zig returns `Data`, which only holds a slice, by
/// value from `Image.renderData`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataRef<'a> {
    Complete(&'a [u8]),
    Pending(usize),
}

impl<'a> DataRef<'a> {
    /// Bytes reserved against the storage limit.
    pub fn len(&self) -> usize {
        match self {
            DataRef::Complete(data) => data.len(),
            DataRef::Pending(expected_len) => *expected_len,
        }
    }

    /// Returns decoded bytes when the payload is complete.
    pub fn bytes(&self) -> Option<&'a [u8]> {
        match self {
            DataRef::Complete(data) => Some(data),
            DataRef::Pending(_) => None,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, DataRef::Pending(_))
    }
}

impl Image {
    /// The pixel data that should be displayed for this image. For an
    /// animated image this is the current animation frame. Otherwise,
    /// and for the root frame, it is the image's own data.
    ///
    /// The result is pending when the displayed pixels have not arrived
    /// yet. That can be true even when `data` is complete, because the
    /// current animation frame may still be pending. Code that draws an
    /// image should check this result rather than `data`:
    ///
    /// ```ignore
    /// let Some(pixels) = image.render_data().bytes() else { return }; // not ready
    /// upload(pixels);
    /// ```
    pub fn render_data(&self) -> DataRef<'_> {
        if let Some(anim) = &self.animation {
            if anim.current_index > 0 {
                let frame = &anim.frames[anim.current_index as usize - 1];
                if frame.is_pending() {
                    return DataRef::Pending(frame.pending_len);
                }
                return DataRef::Complete(&frame.data);
            }
        }

        self.data.as_ref()
    }

    /// The pixel data of the given 1-based animation frame number, or
    /// null if the frame doesn't exist or its pixels have not arrived
    /// yet. Frame 1 (the root frame) always exists as long as the image
    /// data is complete, even for images without animation state.
    ///
    /// The returned slice is owned by the image (or its animation)
    /// and remains valid until the image or frame is mutated.
    pub fn frame_data(&self, number: u32) -> Option<&[u8]> {
        match number {
            0 => None,
            1 => self.data.bytes(),
            _ => {
                let anim = self.animation.as_ref()?;
                // Minus 2 because frame is 1-based and frame 1 is the
                // image base data, so the animation frames start at frame 2.
                let idx = (number - 2) as usize;
                let frame = anim.frames.get(idx)?;
                if frame.is_pending() {
                    return None;
                }
                Some(&frame.data)
            }
        }
    }

    /// Mutable [`Image::frame_data`] (Zig: `@constCast(img.frameData(n))`,
    /// used by the animation commands that compose into stored frames).
    pub fn frame_data_mut(&mut self, number: u32) -> Option<&mut [u8]> {
        match number {
            0 => None,
            1 => match &mut self.data {
                Data::Complete(data) => Some(data),
                Data::Pending(_) => None,
            },
            _ => {
                let anim = self.animation.as_mut()?;
                let idx = (number - 2) as usize;
                let frame = anim.frames.get_mut(idx)?;
                if frame.is_pending() {
                    return None;
                }
                Some(&mut frame.data)
            }
        }
    }

    /// Total bytes of pixel data reserved against the storage limit
    /// for this image: the base data plus any animation frames.
    pub fn storage_size(&self) -> usize {
        let mut total: usize = self.data.len();
        if let Some(anim) = &self.animation {
            total += anim.frame_bytes();
        }
        total
    }

    /// Mostly for logging. A copy of the image metadata without the pixel
    /// data. (Zig's copy shares the animation pointer; the Rust copy
    /// carries no animation since it can't share ownership.)
    pub fn without_data(&self) -> Image {
        Image {
            id: self.id,
            number: self.number,
            width: self.width,
            height: self.height,
            format: self.format,
            compression: self.compression,
            data: match &self.data {
                Data::Complete(_) => Data::Complete(Vec::new()),
                Data::Pending(len) => Data::Pending(*len),
            },
            metadata: self.metadata,
            generation: self.generation,
            transmission: self.transmission,
            animation: None,
        }
    }
}

/// The rect taken up by some image placement, in grid cells. This will
/// be rounded up to the nearest grid cell since we can't place images
/// in partial grid cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub top_left: Pin,
    pub bottom_right: Pin,
}

impl Rect {
    /// Returns true if the grid cell is inside this rectangle. Pin.isBetween
    /// compares page order, so its interior rows intentionally do not constrain
    /// x. Check the column independently and use isBetween only for the row.
    pub fn contains(&self, cell: Pin) -> bool {
        if cell.x < self.top_left.x || cell.x > self.bottom_right.x {
            return false;
        }

        let mut row = cell;
        row.x = self.top_left.x;
        row.is_between(self.top_left, self.bottom_right)
    }
}

/// Returns whether a name can be passed to shm_open. Slashes are left
/// for shm_open to judge since the valid forms are platform specific.
fn valid_shared_memory_name(name: &[u8], name_max: usize) -> bool {
    if name.is_empty() || name.len() > name_max {
        return false;
    }
    if name == b"/" {
        return false;
    }
    memchr::memchr(0, name).is_none()
}

/// Whether `c` is a path separator (`std.fs.path.isSep`).
fn is_sep(c: u8) -> bool {
    if cfg!(windows) {
        c == b'/' || c == b'\\'
    } else {
        c == b'/'
    }
}

/// Returns true if `path` is `dir` or is contained within it, requiring a
/// path-separator boundary so similarly prefixed directories do not match.
fn is_path_in_dir(dir: &[u8], path: &[u8]) -> bool {
    if dir.is_empty() || !path.starts_with(dir) {
        return false;
    }
    if path.len() == dir.len() || is_sep(dir[dir.len() - 1]) {
        return true;
    }
    is_sep(path[dir.len()])
}

/// Returns true if path appears to be in a temporary directory.
/// Copies logic from Kitty.
fn is_path_in_temp_dir(dir: &[u8], path: &[u8]) -> bool {
    if is_path_in_dir(b"/tmp", path) {
        return true;
    }
    if is_path_in_dir(b"/dev/shm", path) {
        return true;
    }
    if is_path_in_dir(dir, path) {
        return true;
    }

    // On macOS /tmp is a symlink to /private/tmp, and the path is
    // canonical (see validatedFilePath), so files in /tmp only
    // match the canonical directory.
    if cfg!(target_os = "macos") && is_path_in_dir(b"/private/tmp", path) {
        return true;
    }

    // The temporary dir is sometimes a symlink. On macOS for
    // example /tmp is /private/var/... On Windows the directory a
    // host passes (typically GetTempPath output) regularly differs
    // from the canonical path in case or uses 8.3 short names, and
    // resolving it covers both.
    let Ok(real_dir) = fs::canonicalize(path_from_bytes(dir)) else {
        return false;
    };
    if is_path_in_dir(&path_to_bytes(&real_dir), path) {
        return true;
    }

    false
}

/// A filesystem path from raw bytes.
#[cfg(unix)]
fn path_from_bytes(path: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::path::PathBuf::from(std::ffi::OsStr::from_bytes(path))
}

#[cfg(not(unix))]
fn path_from_bytes(path: &[u8]) -> std::path::PathBuf {
    std::path::PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

/// The raw bytes of a filesystem path.
#[cfg(unix)]
fn path_to_bytes(path: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_to_bytes(path: &std::path::Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

/// The canonical path of an open file (`std.Io.File.realPath`).
#[cfg(target_os = "linux")]
fn real_path(file: &fs::File) -> std::io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    let link = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    Ok(path_to_bytes(&link))
}

#[cfg(target_os = "macos")]
fn real_path(file: &fs::File) -> std::io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX
    // bytes into buf.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let len = memchr::memchr(0, &buf).unwrap_or(buf.len());
    buf.truncate(len);
    Ok(buf)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn real_path(_file: &fs::File) -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

#[cfg(test)]
pub(crate) mod test_util {
    //! Emulation of Zig's `std.testing.tmpDir`.

    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A temporary directory removed on drop. It lives under
    /// `target/tmp/<sub_path>` relative to the package root (the test
    /// working directory), like Zig's `.zig-cache/tmp/<sub_path>`.
    pub struct TmpDir {
        pub sub_path: String,
        pub path: PathBuf,
    }

    impl TmpDir {
        pub fn new() -> TmpDir {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let sub_path = format!(
                "kitty-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("tmp")
                .join(&sub_path);
            std::fs::create_dir_all(&path).unwrap();
            TmpDir { sub_path, path }
        }

        /// The path relative to the test working directory.
        pub fn relative(&self) -> String {
            format!("target/tmp/{}", self.sub_path)
        }

        pub fn write_file(&self, sub_path: &str, data: &[u8]) {
            std::fs::write(self.path.join(sub_path), data).unwrap();
        }

        /// The canonical path of the directory as bytes.
        pub fn real_path(&self) -> Vec<u8> {
            super::path_to_bytes(&std::fs::canonicalize(&self.path).unwrap())
        }

        /// The canonical path of a file in the directory as bytes.
        pub fn real_path_file(&self, sub_path: &str) -> Vec<u8> {
            super::path_to_bytes(&std::fs::canonicalize(self.path.join(sub_path)).unwrap())
        }

        pub fn exists(&self, sub_path: &str) -> bool {
            self.path.join(sub_path).symlink_metadata().is_ok()
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::TmpDir;
    use super::*;
    use crate::terminal::kitty::graphics_command::{Command, Control, Quiet};

    const RGB_NONE_20X15: &[u8] = include_bytes!("testdata/image-rgb-none-20x15-2147483647-raw.data");
    const RGB_ZLIB_128X96: &[u8] =
        include_bytes!("testdata/image-rgb-zlib_deflate-128x96-2147483647-raw.data");
    const PNG_NONE_50X76: &[u8] = include_bytes!("testdata/image-png-none-50x76-2147483647-raw.data");
    const PNG_ZLIB_50X76: &[u8] =
        include_bytes!("testdata/image-png-zlib_deflate-50x76-2147483647-raw.data");

    fn transmit_cmd(t: Transmission, data: &[u8]) -> Command {
        Command {
            control: Control::Transmit(t),
            quiet: Quiet::No,
            data: data.to_vec(),
        }
    }

    fn png_read_lock() -> std::sync::RwLockReadGuard<'static, ()> {
        sys::DECODE_PNG_TEST_LOCK
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn temporary_file_path_must_be_inside_directory() {
        assert!(is_path_in_dir(b"/tmp", b"/tmp/tty-graphics-protocol-image.data"));
        assert!(is_path_in_dir(b"/tmp/", b"/tmp/tty-graphics-protocol-image.data"));
        assert!(is_path_in_dir(b"/tmp", b"/tmp"));

        assert!(!is_path_in_dir(b"", b"/tmp/tty-graphics-protocol-image.data"));
        assert!(!is_path_in_dir(b"/tmp", b"/tmpX/tty-graphics-protocol-image.data"));
        assert!(!is_path_in_dir(b"/dev/shm", b"/dev/shm-evil/tty-graphics-protocol-image.data"));
        assert!(!is_path_in_dir(
            b"/custom/tmp",
            b"/custom/tmp-suffix/tty-graphics-protocol-image.data"
        ));
    }

    #[test]
    fn shared_memory_names() {
        assert!(valid_shared_memory_name(b"/kitty", 8));
        assert!(valid_shared_memory_name(b"/1234567", 8));
        assert!(valid_shared_memory_name(b"kitty", 8));
        assert!(valid_shared_memory_name(b"TEAMID.group/kitty", 31));
        assert!(valid_shared_memory_name(b"/kitty/image", 16));

        assert!(!valid_shared_memory_name(b"", 8));
        assert!(!valid_shared_memory_name(b"/", 8));
        assert!(!valid_shared_memory_name(b"/kitty\x00image", 16));
        assert!(!valid_shared_memory_name(b"kitty\x00image", 16));
        assert!(!valid_shared_memory_name(b"/12345678", 8));
    }

    #[cfg(not(any(target_os = "android", windows)))]
    #[test]
    fn image_load_rejects_shared_memory_names_containing_nul() {
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium: Medium::SharedMemory,
                width: 1,
                height: 1,
                image_id: 31,
                ..Default::default()
            },
            b"/kitty\x00image",
        );

        assert_eq!(
            Err(Error::InvalidData),
            LoadingImage::init(
                &cmd,
                &Limits {
                    file: false,
                    temporary_file: TemporaryFile::Disabled,
                    shared_memory: true,
                }
            )
        );
    }

    // FreeBSD's shm_open rejects names without a leading slash with
    // EINVAL, so there is no such object for us to load.
    #[cfg(not(any(target_os = "android", windows, target_os = "freebsd")))]
    #[test]
    fn image_load_accepts_shared_memory_names_without_a_leading_slash() {
        use std::ffi::CString;

        // Sandboxed macOS apps can only create names of this form. The pid
        // keeps concurrent test runs apart; Darwin limits names to 31 bytes.
        let name = format!(
            "{}{}",
            if cfg!(target_os = "macos") {
                "ghostty.test/shm-"
            } else {
                "ghostty-test-shm-"
            },
            std::process::id()
        );
        let namez = CString::new(name.clone()).unwrap();
        let data: &[u8] = b"\x01\x02\x03";

        // SAFETY: valid NUL-terminated name.
        let fd = unsafe {
            libc::shm_open(
                namez.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600 as libc::mode_t,
            )
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::ENOSYS) => return,
                _ => panic!("unable to create shared memory {name}: {err}"),
            }
        }
        struct Cleanup(libc::c_int, CString);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                // SAFETY: closing our own fd and unlinking our own name.
                unsafe {
                    libc::shm_unlink(self.1.as_ptr());
                    libc::close(self.0);
                }
            }
        }
        let _cleanup = Cleanup(fd, namez.clone());
        // SAFETY: fd is valid.
        assert_eq!(0, unsafe { libc::ftruncate(fd, data.len() as libc::off_t) });
        {
            // macOS does not support write(2) on shared memory, so map it.
            // SAFETY: mapping data.len() bytes of a valid fd.
            let map = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    data.len(),
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            assert_ne!(map, libc::MAP_FAILED);
            // SAFETY: the mapping is data.len() bytes long.
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), map as *mut u8, data.len());
                libc::munmap(map, data.len());
            }
        }

        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium: Medium::SharedMemory,
                width: 1,
                height: 1,
                image_id: 31,
                ..Default::default()
            },
            name.as_bytes(),
        );

        let loading = LoadingImage::init(
            &cmd,
            &Limits {
                file: false,
                temporary_file: TemporaryFile::Disabled,
                shared_memory: true,
            },
        )
        .unwrap();
        assert_eq!(data, &loading.data[..]);

        // The terminal must unlink the object after reading it. fstat can't
        // tell us this since macOS always reports st_nlink 0 for shared memory.
        // SAFETY: valid NUL-terminated name.
        let reopened = unsafe { libc::shm_open(namez.as_ptr(), libc::O_RDONLY, 0) };
        let errno = std::io::Error::last_os_error().raw_os_error();
        if reopened >= 0 {
            // SAFETY: closing the fd we just opened.
            unsafe { libc::close(reopened) };
        }
        assert!(reopened < 0);
        assert_eq!(Some(libc::ENOENT), errno);
    }

    fn loading_1x1(format: Format, width: u32, height: u32) -> LoadingImage {
        LoadingImage::new(
            Image {
                width,
                height,
                format,
                ..Default::default()
            },
            Quiet::No,
        )
    }

    #[test]
    fn shared_memory_range_with_offset_and_size() {
        let loading = loading_1x1(Format::Rgb, 1, 1);

        let explicit = loading
            .shared_memory_range(
                &Transmission {
                    offset: 2,
                    size: 3,
                    ..Default::default()
                },
                5,
            )
            .unwrap();
        assert_eq!(2usize, explicit.start);
        assert_eq!(5usize, explicit.end);

        let implicit = loading
            .shared_memory_range(
                &Transmission {
                    offset: 2,
                    ..Default::default()
                },
                5,
            )
            .unwrap();
        assert_eq!(2usize, implicit.start);
        assert_eq!(5usize, implicit.end);
    }

    #[test]
    fn shared_memory_range_rejects_out_of_bounds_offset() {
        let loading = loading_1x1(Format::Rgb, 1, 1);
        assert_eq!(
            Err(Error::InvalidData),
            loading.shared_memory_range(
                &Transmission {
                    offset: 4,
                    ..Default::default()
                },
                3
            )
        );
    }

    #[test]
    fn shared_memory_range_validates_dimensions_before_multiplication() {
        let loading = loading_1x1(Format::Rgba, u32::MAX, u32::MAX);
        assert_eq!(
            Err(Error::DimensionsTooLarge),
            loading.shared_memory_range(&Transmission::default(), 1)
        );
    }

    // This specifically tests we ALLOW invalid RGB data because Kitty
    // documents that this should work.
    #[test]
    fn image_load_with_invalid_rgb_data() {
        // <ESC>_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA<ESC>\
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                width: 1,
                height: 1,
                image_id: 31,
                ..Default::default()
            },
            b"AAAA",
        );
        let _loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();
    }

    #[test]
    fn image_load_with_image_too_wide() {
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                width: MAX_DIMENSION + 1,
                height: 1,
                image_id: 31,
                ..Default::default()
            },
            b"AAAA",
        );
        let mut loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();
        assert_eq!(Err(Error::DimensionsTooLarge), loading.complete());
    }

    #[test]
    fn image_load_with_image_too_tall() {
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                height: MAX_DIMENSION + 1,
                width: 1,
                image_id: 31,
                ..Default::default()
            },
            b"AAAA",
        );
        let mut loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();
        assert_eq!(Err(Error::DimensionsTooLarge), loading.complete());
    }

    #[test]
    fn image_load_rgb_zlib_compressed_direct() {
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium: Medium::Direct,
                compression: Compression::ZlibDeflate,
                height: 96,
                width: 128,
                image_id: 31,
                ..Default::default()
            },
            RGB_ZLIB_128X96,
        );
        let mut loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();
        let img = loading.complete().unwrap();

        // should be decompressed
        assert!(img.compression == Compression::None);
    }

    #[test]
    fn image_load_rgb_not_compressed_direct() {
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium: Medium::Direct,
                compression: Compression::None,
                width: 20,
                height: 15,
                image_id: 31,
                ..Default::default()
            },
            RGB_NONE_20X15,
        );
        let mut loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();
        let img = loading.complete().unwrap();

        // should be decompressed
        assert!(img.compression == Compression::None);
    }

    #[test]
    fn image_load_rgb_zlib_compressed_direct_chunked() {
        let data = RGB_ZLIB_128X96;

        // Setup our initial chunk
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium: Medium::Direct,
                compression: Compression::ZlibDeflate,
                height: 96,
                width: 128,
                image_id: 31,
                more_chunks: true,
                ..Default::default()
            },
            &data[..1024],
        );
        let mut loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();

        // Read our remaining chunks
        for chunk in data[1024..].chunks(1024) {
            loading.add_data(chunk).unwrap();
        }

        // Complete
        let img = loading.complete().unwrap();
        assert!(img.compression == Compression::None);
    }

    #[test]
    fn image_load_rgb_zlib_compressed_direct_chunked_with_zero_initial_chunk() {
        let data = RGB_ZLIB_128X96;

        // Setup our initial chunk
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium: Medium::Direct,
                compression: Compression::ZlibDeflate,
                height: 96,
                width: 128,
                image_id: 31,
                more_chunks: true,
                ..Default::default()
            },
            b"",
        );
        let mut loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();

        // Read our remaining chunks
        for chunk in data.chunks(1024) {
            loading.add_data(chunk).unwrap();
        }

        // Complete
        let img = loading.complete().unwrap();
        assert!(img.compression == Compression::None);
    }

    fn rgb_20x15(medium: Medium, path: &[u8]) -> Command {
        transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium,
                compression: Compression::None,
                width: 20,
                height: 15,
                image_id: 31,
                ..Default::default()
            },
            path,
        )
    }

    #[test]
    fn image_load_temporary_file_without_correct_path() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("image.data", RGB_NONE_20X15);
        let path = tmp_dir.real_path_file("image.data");

        let cmd = rgb_20x15(Medium::TemporaryFile, &path);
        assert_eq!(
            Err(Error::TemporaryFileNotNamedCorrectly),
            LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&tmp_dir.real_path()))
        );

        // Temporary file should still be there
        assert!(tmp_dir.exists("image.data"));
    }

    #[test]
    fn image_load_temporary_file_outside_directory_prefix_is_rejected() {
        let tmp_dir = TmpDir::new();
        std::fs::create_dir(tmp_dir.path.join("temp")).unwrap();
        std::fs::create_dir(tmp_dir.path.join("temp-suffix")).unwrap();

        let filename = "tty-graphics-protocol-image.data";
        tmp_dir.write_file(&format!("temp-suffix/{filename}"), RGB_NONE_20X15);

        let trusted_path = tmp_dir.real_path_file("temp");
        let outside_path = tmp_dir.real_path_file(&format!("temp-suffix/{filename}"));

        let cmd = rgb_20x15(Medium::TemporaryFile, &outside_path);
        assert_eq!(
            Err(Error::TemporaryFileNotInTempDir),
            LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&trusted_path))
        );

        // Rejection must happen before temporary-file cleanup is armed.
        assert!(tmp_dir.exists(&format!("temp-suffix/{filename}")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn image_load_temporary_file_in_tmp_on_macos() {
        // /tmp is a symlink to /private/tmp on macOS, so the canonical path
        // of a file in it does not start with /tmp.

        // The configured temporary directory does not contain /tmp, so the
        // file can only be accepted through the /tmp root.
        let tmp_dir = TmpDir::new();
        let dir_path = tmp_dir.real_path();

        let path = format!(
            "/tmp/ghostty-test-{}-tty-graphics-protocol-image.data",
            tmp_dir.sub_path
        );
        std::fs::write(&path, RGB_NONE_20X15).unwrap();
        struct Rm(String);
        impl Drop for Rm {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _rm = Rm(path.clone());

        let cmd = rgb_20x15(Medium::TemporaryFile, path.as_bytes());
        let mut loading =
            LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&dir_path)).unwrap();
        let img = loading.complete().unwrap();
        assert!(img.compression == Compression::None);

        // Temporary file should be gone
        assert!(std::fs::metadata(&path).is_err());
    }

    #[test]
    fn image_load_rgb_not_compressed_temporary_file() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("tty-graphics-protocol-image.data", RGB_NONE_20X15);
        let path = tmp_dir.real_path_file("tty-graphics-protocol-image.data");

        let cmd = rgb_20x15(Medium::TemporaryFile, &path);
        let mut loading =
            LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&tmp_dir.real_path())).unwrap();
        let img = loading.complete().unwrap();
        assert!(img.compression == Compression::None);

        // Temporary file should be gone
        assert!(!tmp_dir.exists("tty-graphics-protocol-image.data"));
    }

    #[test]
    fn image_load_rgb_not_compressed_regular_file() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("image.data", RGB_NONE_20X15);
        let path = tmp_dir.real_path_file("image.data");

        let cmd = rgb_20x15(Medium::File, &path);
        let mut loading =
            LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&tmp_dir.real_path())).unwrap();
        let img = loading.complete().unwrap();
        assert!(img.compression == Compression::None);
        assert!(tmp_dir.exists("image.data"));
    }

    #[test]
    fn image_load_regular_file_size_reads_exactly_requested_bytes() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("image.data", &[1, 2, 3, 4, 5, 6]);
        let path = tmp_dir.real_path_file("image.data");

        struct Case {
            offset: u32,
            expected: [u8; 3],
        }
        let cases = [
            Case {
                offset: 0,
                expected: [1, 2, 3],
            },
            Case {
                offset: 3,
                expected: [4, 5, 6],
            },
        ];
        for case in cases {
            let cmd = transmit_cmd(
                Transmission {
                    format: Format::Rgb,
                    medium: Medium::File,
                    width: 1,
                    height: 1,
                    size: 3,
                    offset: case.offset,
                    image_id: 31,
                    ..Default::default()
                },
                &path,
            );

            let mut loading = LoadingImage::init(
                &cmd,
                &Limits {
                    file: true,
                    temporary_file: TemporaryFile::Disabled,
                    shared_memory: false,
                },
            )
            .unwrap();
            let img = loading.complete().unwrap();

            assert_eq!(&case.expected[..], img.data.bytes().unwrap());
        }
    }

    #[test]
    fn image_load_regular_file_size_rejects_short_data() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("image.data", &[1, 2]);
        let path = tmp_dir.real_path_file("image.data");
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium: Medium::File,
                width: 1,
                height: 1,
                size: 3,
                image_id: 31,
                ..Default::default()
            },
            &path,
        );

        assert_eq!(
            Err(Error::InvalidData),
            LoadingImage::init(
                &cmd,
                &Limits {
                    file: true,
                    temporary_file: TemporaryFile::Disabled,
                    shared_memory: false,
                }
            )
        );
    }

    #[test]
    fn image_load_rgb_not_compressed_relative_regular_file() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("image.data", RGB_NONE_20X15);

        let rel = format!("{}/image.data", tmp_dir.relative());
        let cmd = rgb_20x15(Medium::File, rel.as_bytes());
        let mut loading = LoadingImage::init(
            &cmd,
            &Limits {
                file: true,
                temporary_file: TemporaryFile::Disabled,
                shared_memory: false,
            },
        )
        .unwrap();
        let img = loading.complete().unwrap();
        assert!(img.compression == Compression::None);
    }

    #[cfg(unix)]
    #[test]
    fn image_load_blocklist_applies_to_opened_file_after_symlink_swap() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("safe.data", b"safe");
        let link = tmp_dir.path.join("image.data");
        std::os::unix::fs::symlink("/dev/null", &link).unwrap();

        // Pin the blocked file, then simulate the cooperating process replacing
        // the path with a safe target before validation.
        let blocked_file = fs::File::open(&link).unwrap();
        let tmp_link = tmp_dir.path.join("image.data.tmp");
        std::os::unix::fs::symlink("safe.data", &tmp_link).unwrap();
        fs::rename(&tmp_link, &link).unwrap();

        assert_eq!(
            Err(Error::InvalidData),
            LoadingImage::validated_file_path(&blocked_file)
        );

        // The pathname now resolves to the safe replacement, demonstrating that
        // the rejection above came from the already-open file handle.
        let safe_file = fs::File::open(&link).unwrap();
        let _ = LoadingImage::validated_file_path(&safe_file).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn image_load_windows_unc_path_is_rejected_before_open() {
        // Opening a UNC path would resolve the host name and authenticate to
        // it over SMB, so the rejection has to happen on the raw path. The
        // host name is in the reserved .invalid TLD so it can never resolve.
        let paths: [&[u8]; 3] = [
            b"\\\\nonexistent-host.invalid\\share\\tty-graphics-protocol-image.data",
            b"//nonexistent-host.invalid/share/tty-graphics-protocol-image.data",
            b"\\\\?\\UNC\\nonexistent-host.invalid\\share\\image.data",
        ];
        for path in paths {
            let cmd = rgb_20x15(Medium::File, path);
            assert_eq!(
                Err(Error::InvalidData),
                LoadingImage::init(
                    &cmd,
                    &Limits {
                        file: true,
                        temporary_file: TemporaryFile::Disabled,
                        shared_memory: false,
                    }
                )
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn image_load_windows_device_namespace_paths_are_rejected_before_open() {
        let tmp_dir = TmpDir::new();
        let filename = "tty-graphics-protocol-image.data";
        tmp_dir.write_file(filename, RGB_NONE_20X15);

        let dir_path = tmp_dir.real_path();
        let real_path = tmp_dir.real_path_file(filename);

        // The verbatim, local device and NT namespace spellings all resolve
        // to the same real file, so rejecting them proves the check runs on
        // the raw path rather than on the canonical one.
        let prefixes: [&[u8]; 3] = [b"\\\\?\\", b"\\\\.\\", b"\\??\\"];
        for prefix in prefixes {
            let path = [prefix, &real_path[..]].concat();
            for medium in [Medium::File, Medium::TemporaryFile] {
                let cmd = rgb_20x15(medium, &path);
                assert_eq!(
                    Err(Error::InvalidData),
                    LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&dir_path))
                );
            }
        }

        // A named pipe reached through the local device namespace.
        {
            let cmd = rgb_20x15(Medium::File, b"\\\\.\\pipe\\ghostty-kitty-graphics-test");
            assert_eq!(
                Err(Error::InvalidData),
                LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&dir_path))
            );
        }

        // Nothing above reached the temporary file deletion.
        assert!(tmp_dir.exists(filename));
    }

    #[cfg(windows)]
    #[test]
    fn image_load_windows_reserved_device_names_are_rejected_before_open() {
        let tmp_dir = TmpDir::new();
        let dir_path = tmp_dir.real_path();

        let names = ["CON", "NUL.data", "com1", "LPT9.png", "AUX", "PRN."];
        for name in names {
            let path = [&dir_path[..], b"\\", name.as_bytes()].concat();
            let cmd = rgb_20x15(Medium::File, &path);
            assert_eq!(
                Err(Error::InvalidData),
                LoadingImage::init(
                    &cmd,
                    &Limits {
                        file: true,
                        temporary_file: TemporaryFile::Disabled,
                        shared_memory: false,
                    }
                )
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn image_load_windows_local_file_accepted_in_forward_slash_and_upper_case_spellings() {
        let tmp_dir = TmpDir::new();
        let filename = "image.data";
        tmp_dir.write_file(filename, RGB_NONE_20X15);
        let real_path = tmp_dir.real_path_file(filename);

        // Both spellings open the same file; the canonical path the checks
        // see is the backslash, on-disk case form either way.
        let forward: Vec<u8> = real_path
            .iter()
            .map(|&c| if c == b'\\' { b'/' } else { c })
            .collect();
        let upper = real_path.to_ascii_uppercase();

        for path in [forward, upper] {
            let cmd = rgb_20x15(Medium::File, &path);
            let mut loading = LoadingImage::init(
                &cmd,
                &Limits {
                    file: true,
                    temporary_file: TemporaryFile::Disabled,
                    shared_memory: false,
                },
            )
            .unwrap();
            let img = loading.complete().unwrap();
            assert!(img.compression == Compression::None);
        }

        assert!(tmp_dir.exists(filename));
    }

    #[cfg(windows)]
    #[test]
    fn image_load_windows_temporary_file_with_differently_spelled_directory() {
        let tmp_dir = TmpDir::new();
        let filename = "tty-graphics-protocol-image.data";
        tmp_dir.write_file(filename, RGB_NONE_20X15);

        let dir_path = tmp_dir.real_path();
        let real_path = tmp_dir.real_path_file(filename);

        // Hosts hand over GetTempPath output, which regularly differs from
        // the canonical path in case (`C:\WINDOWS\TEMP\`), so the directory
        // prefix check only passes through the real-path fallback.
        let dir_upper = dir_path.to_ascii_uppercase();
        let path_upper = real_path.to_ascii_uppercase();
        assert!(!is_path_in_dir(&dir_upper, &real_path) || dir_upper == dir_path);

        let cmd = rgb_20x15(Medium::TemporaryFile, &path_upper);
        let mut loading = LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&dir_upper)).unwrap();
        let img = loading.complete().unwrap();
        assert!(img.compression == Compression::None);

        // Temporary file should be gone
        assert!(!tmp_dir.exists(filename));
    }

    #[cfg(windows)]
    #[test]
    fn image_load_windows_canonical_path_check_accepts_a_local_file_opened_through_a_device_spelling(
    ) {
        let tmp_dir = TmpDir::new();
        let filename = "image.data";
        tmp_dir.write_file(filename, b"safe");
        let real_path = tmp_dir.real_path_file(filename);

        // readFile refuses this spelling before the open, but the post-open
        // check works from the canonical path and so does not care how the
        // file was reached: `\\.\C:\...` canonicalizes back to `C:\...`.
        let device_path = [&b"\\\\.\\"[..], &real_path[..]].concat();
        let file = fs::File::open(path_from_bytes(&device_path)).unwrap();

        let canon = LoadingImage::validated_file_path(&file).unwrap();
        assert_eq!(real_path, canon);
    }

    #[cfg(windows)]
    #[test]
    fn image_load_windows_canonical_path_check_rejects_a_file_reached_through_a_unc_share() {
        let tmp_dir = TmpDir::new();
        let filename = "image.data";
        tmp_dir.write_file(filename, b"safe");
        let real_path = tmp_dir.real_path_file(filename);
        assert!(kitty_windows::is_drive_absolute(&real_path));

        // Reach the same local file through the loopback administrative
        // share, which is how a junction or symlink into a share would look
        // to the post-open check. The share needs the server service and an
        // administrative token, so the test is skipped when the open fails.
        let unc_path = format!(
            "\\\\localhost\\{}$\\{}",
            real_path[0] as char,
            String::from_utf8_lossy(&real_path[3..])
        );
        let Ok(file) = fs::File::open(&unc_path) else {
            return;
        };

        // (Zig: error.NotDriveAbsolute; Rust maps it to InvalidData.)
        assert_eq!(
            Err(Error::InvalidData),
            LoadingImage::validated_file_path(&file)
        );
    }

    #[test]
    fn image_load_png_not_compressed_regular_file() {
        let _lock = png_read_lock();
        if sys::decode_png().is_none() {
            return;
        }

        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("tty-graphics-protocol-image.data", PNG_NONE_50X76);
        let path = tmp_dir.real_path_file("tty-graphics-protocol-image.data");

        let cmd = transmit_cmd(
            Transmission {
                format: Format::Png,
                medium: Medium::File,
                compression: Compression::None,
                width: 0,
                height: 0,
                image_id: 31,
                ..Default::default()
            },
            &path,
        );
        let mut loading =
            LoadingImage::init(&cmd, &Limits::all_with_temp_dir(&tmp_dir.real_path())).unwrap();
        let img = loading.complete().unwrap();
        assert!(img.compression == Compression::None);
        assert!(img.format == Format::Rgba);
        assert!(tmp_dir.exists("tty-graphics-protocol-image.data"));
    }

    #[test]
    fn image_load_png_zlib_compressed_direct() {
        let _lock = png_read_lock();
        if sys::decode_png().is_none() {
            return;
        }

        let cmd = transmit_cmd(
            Transmission {
                format: Format::Png,
                medium: Medium::Direct,
                compression: Compression::ZlibDeflate,
                image_id: 31,
                ..Default::default()
            },
            PNG_ZLIB_50X76,
        );
        let mut loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();
        let img = loading.complete().unwrap();

        assert_eq!(Compression::None, img.compression);
        assert_eq!(Format::Rgba, img.format);
        assert_eq!(50u32, img.width);
        assert_eq!(76u32, img.height);
    }

    #[test]
    fn image_load_png_rejects_oversized_decoder_allocation() {
        fn oversized_decoder(_: &[u8]) -> Result<sys::Image, sys::DecodeError> {
            let data = sys::decode_alloc(MAX_SIZE + 1)?;
            Ok(sys::Image {
                width: 1,
                height: 1,
                data,
            })
        }

        let _lock = sys::DECODE_PNG_TEST_LOCK
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let original_decode_png = sys::decode_png();
        sys::set_decode_png(Some(oversized_decoder));
        struct Restore(Option<sys::DecodePngFn>);
        impl Drop for Restore {
            fn drop(&mut self) {
                sys::set_decode_png(self.0);
            }
        }
        let _restore = Restore(original_decode_png);

        // The size limiter should reject the decoder's request before it
        // reaches the underlying allocator. (Zig additionally checks with a
        // FailingAllocator that nothing was allocated at all.)
        let mut loading = LoadingImage::new(
            Image {
                format: Format::Png,
                ..Default::default()
            },
            Quiet::No,
        );

        assert_eq!(Err(Error::InvalidData), loading.complete());
    }

    #[test]
    fn image_load_png_rejects_oversized_wuffs_image_before_allocation() {
        let _lock = png_read_lock();
        if sys::decode_png().is_none() {
            return;
        }

        // Turn the small test PNG into a 32768x32767 image. Its decoded RGBA
        // size is just under Wuffs' 4 GiB package limit but over Kitty's 400 MiB
        // limit, which previously allowed the large allocation to happen first.
        let mut data = PNG_NONE_50X76.to_vec();
        data[16..20].copy_from_slice(&32768u32.to_be_bytes());
        data[20..24].copy_from_slice(&32767u32.to_be_bytes());
        let crc = crc32(&data[12..29]);
        data[29..33].copy_from_slice(&crc.to_be_bytes());

        let cmd = transmit_cmd(
            Transmission {
                format: Format::Png,
                medium: Medium::Direct,
                ..Default::default()
            },
            &data,
        );
        let mut loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();

        assert_eq!(Err(Error::InvalidData), loading.complete());
    }

    /// `std.hash.Crc32` (IEEE).
    fn crc32(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    #[test]
    fn limits_direct_medium_always_allowed() {
        let cmd = transmit_cmd(
            Transmission {
                format: Format::Rgb,
                medium: Medium::Direct,
                width: 1,
                height: 1,
                image_id: 31,
                ..Default::default()
            },
            b"AAAA",
        );

        // Direct medium should work even with the most restrictive limits
        let _loading = LoadingImage::init(&cmd, &Limits::DIRECT).unwrap();
    }

    #[test]
    fn limits_file_medium_blocked_by_limits() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("image.data", RGB_NONE_20X15);
        let path = tmp_dir.real_path_file("image.data");

        let cmd = rgb_20x15(Medium::File, &path);
        assert_eq!(
            Err(Error::UnsupportedMedium),
            LoadingImage::init(&cmd, &Limits::DIRECT)
        );
    }

    #[test]
    fn limits_file_medium_allowed_by_limits() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("image.data", RGB_NONE_20X15);
        let path = tmp_dir.real_path_file("image.data");

        let cmd = rgb_20x15(Medium::File, &path);
        let _loading = LoadingImage::init(
            &cmd,
            &Limits {
                file: true,
                temporary_file: TemporaryFile::Disabled,
                shared_memory: false,
            },
        )
        .unwrap();
    }

    #[test]
    fn limits_temporary_file_medium_blocked_by_limits() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("tty-graphics-protocol-image.data", RGB_NONE_20X15);
        let path = tmp_dir.real_path_file("tty-graphics-protocol-image.data");

        let cmd = rgb_20x15(Medium::TemporaryFile, &path);
        assert_eq!(
            Err(Error::UnsupportedMedium),
            LoadingImage::init(
                &cmd,
                &Limits {
                    file: true,
                    temporary_file: TemporaryFile::Disabled,
                    shared_memory: true,
                }
            )
        );

        // File should still exist since we blocked before reading
        assert!(tmp_dir.exists("tty-graphics-protocol-image.data"));
    }

    #[test]
    fn limits_temporary_file_medium_allowed_by_limits() {
        let tmp_dir = TmpDir::new();
        tmp_dir.write_file("tty-graphics-protocol-image.data", RGB_NONE_20X15);
        let path = tmp_dir.real_path_file("tty-graphics-protocol-image.data");

        let cmd = rgb_20x15(Medium::TemporaryFile, &path);
        let _loading = LoadingImage::init(
            &cmd,
            &Limits {
                file: false,
                temporary_file: TemporaryFile::Enabled {
                    directory: tmp_dir.real_path(),
                },
                shared_memory: false,
            },
        )
        .unwrap();
    }
}
