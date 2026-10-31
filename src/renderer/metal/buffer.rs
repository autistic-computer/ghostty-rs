//! Port of `src/renderer/metal/buffer.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)

use std::ffi::c_ulong;
use std::fmt;
use std::marker::PhantomData;

use crate::pkg::macos;

use super::api as mtl;
use super::objc;

const LOG: &str = "metal";

/// Options for initializing a buffer.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// MTLDevice
    pub device: objc::Object,
    pub resource_options: mtl::MTLResourceOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    MetalFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MetalFailed")
    }
}

impl std::error::Error for Error {}

/// Metal data storage for a certain set of equal types. This is usually
/// used for vertex buffers, etc. This helpful wrapper makes it easy to
/// prealloc, shrink, grow, sync, buffers with Metal.
#[derive(Debug)]
pub struct Buffer<T: Copy> {
    /// The options this buffer was initialized with.
    pub opts: Options,

    /// The underlying MTLBuffer object.
    pub buffer: objc::Object,

    /// The allocated length of the buffer.
    /// Note that this is the number
    /// of `T`s not the size in bytes.
    pub len: usize,

    _marker: PhantomData<T>,
}

impl<T: Copy> Buffer<T> {
    fn new_buffer(opts: &Options, len: usize) -> objc::Object {
        // SAFETY: valid MTLDevice.
        unsafe {
            opts.device.msg_send(
                objc::sel(c"newBufferWithLength:options:"),
                (
                    (len * std::mem::size_of::<T>()) as c_ulong,
                    opts.resource_options.to_bits(),
                ),
            )
        }
    }

    /// Initialize a buffer with the given length pre-allocated.
    pub fn init(opts: Options, len: usize) -> Result<Self, Error> {
        let buffer = Self::new_buffer(&opts, len);

        Ok(Buffer {
            buffer,
            opts,
            len,
            _marker: PhantomData,
        })
    }

    /// Init the buffer filled with the given data.
    pub fn init_fill(opts: Options, data: &[T]) -> Result<Self, Error> {
        // SAFETY: valid MTLDevice; `data` is valid for its byte length.
        let buffer: objc::Object = unsafe {
            opts.device.msg_send(
                objc::sel(c"newBufferWithBytes:length:options:"),
                (
                    data.as_ptr() as *const std::ffi::c_void,
                    std::mem::size_of_val(data) as c_ulong,
                    opts.resource_options.to_bits(),
                ),
            )
        };

        Ok(Buffer {
            buffer,
            opts,
            len: data.len(),
            _marker: PhantomData,
        })
    }

    pub fn deinit(&self) {
        // SAFETY: we own the buffer.
        unsafe { self.buffer.msg_send::<(), _>(objc::sel(c"release"), ()) };
    }

    /// Reallocate (to double the required size) if `req_bytes` doesn't fit,
    /// then return the buffer contents pointer.
    fn prepare(&mut self, total_len: usize) -> Result<*mut u8, Error> {
        // If we need more bytes than our buffer has, we need to reallocate.
        let req_bytes = total_len * std::mem::size_of::<T>();
        // SAFETY: valid MTLBuffer.
        let avail_bytes: c_ulong = unsafe { self.buffer.get_property(c"length") };
        if req_bytes as c_ulong > avail_bytes {
            // Deallocate previous buffer
            self.deinit();

            // Allocate a new buffer with enough to hold double what we require.
            self.len = total_len * 2;
            self.buffer = Self::new_buffer(&self.opts, self.len);
        }

        // We can fit within the buffer so we can just replace bytes.
        // SAFETY: valid MTLBuffer.
        let ptr: *mut u8 = unsafe { self.buffer.msg_send(objc::sel(c"contents"), ()) };
        if ptr.is_null() {
            log::warn!(target: LOG, "buffer contents ptr is null");
            return Err(Error::MetalFailed);
        }
        Ok(ptr)
    }

    /// If we're using the managed resource storage mode, then
    /// we need to signal Metal to synchronize the buffer data.
    ///
    /// Ref: https://developer.apple.com/documentation/metal/synchronizing-a-managed-resource-in-macos?language=objc
    fn did_modify(&self, req_bytes: usize) {
        if self.opts.resource_options.storage_mode == mtl::StorageMode::Managed {
            // SAFETY: valid MTLBuffer.
            unsafe {
                self.buffer.msg_send::<(), _>(
                    objc::sel(c"didModifyRange:"),
                    (macos::foundation::Range::init(0, req_bytes),),
                )
            };
        }
    }

    /// Sync new contents to the buffer. The data is expected to be the
    /// complete contents of the buffer. If the amount of data is larger
    /// than the buffer length, the buffer will be reallocated.
    ///
    /// If the amount of data is smaller than the buffer length, the
    /// remaining data in the buffer is left untouched.
    pub fn sync(&mut self, data: &[T]) -> Result<(), Error> {
        let req_bytes = std::mem::size_of_val(data);
        let dst = self.prepare(data.len())?;

        // SAFETY: the buffer holds at least `req_bytes` bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), dst, req_bytes) };

        self.did_modify(req_bytes);
        Ok(())
    }

    /// Like Buffer.sync but takes data from an array of ArrayLists,
    /// rather than a single array. Returns the number of items synced.
    pub fn sync_from_array_lists(&mut self, lists: &[Vec<T>]) -> Result<usize, Error> {
        let total_len: usize = lists.iter().map(Vec::len).sum();

        let req_bytes = total_len * std::mem::size_of::<T>();
        let dst = self.prepare(total_len)?;

        let mut i: usize = 0;
        for list in lists {
            let n = std::mem::size_of_val(list.as_slice());
            // SAFETY: the buffer holds at least `req_bytes` bytes.
            unsafe { std::ptr::copy_nonoverlapping(list.as_ptr().cast::<u8>(), dst.add(i), n) };
            i += n;
        }

        self.did_modify(req_bytes);
        Ok(total_len)
    }
}
