//! Port of `pkg/harfbuzz/blob.zig`.
//!
//! STATUS: ported

use std::os::raw::{c_char, c_uint, c_void};
use std::ptr::NonNull;

use super::c;
use super::errors::Error;

/// Data type holding the memory modes available to client programs.
///
/// Regarding these various memory-modes:
///
///   - In no case shall the HarfBuzz client modify memory that is passed to
///     HarfBuzz in a blob. If there is any such possibility,
///     HB_MEMORY_MODE_DUPLICATE should be used such that HarfBuzz makes a
///     copy immediately,
///
///   - Use HB_MEMORY_MODE_READONLY otherwise, unless you really really really
///     know what you are doing,
///
///   - HB_MEMORY_MODE_WRITABLE is appropriate if you really made a copy of
///     data solely for the purpose of passing to HarfBuzz and doing that
///     just once (no reuse!),
///
///   - If the font is mmap()ed, it's okay to use
///     HB_MEMORY_READONLY_MAY_MAKE_WRITABLE , however, using that mode
///     correctly is very tricky. Use HB_MEMORY_MODE_READONLY instead.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMode {
    /// HarfBuzz immediately makes a copy of the data.
    Duplicate = c::HB_MEMORY_MODE_DUPLICATE,

    /// HarfBuzz client will never modify the data, and HarfBuzz will never
    /// modify the data.
    Readonly = c::HB_MEMORY_MODE_READONLY,

    /// HarfBuzz client made a copy of the data solely for HarfBuzz, so
    /// HarfBuzz may modify the data.
    Writable = c::HB_MEMORY_MODE_WRITABLE,

    /// See above
    ReadonlyMayMakeWritable = c::HB_MEMORY_MODE_READONLY_MAY_MAKE_WRITABLE,
}

/// Blobs wrap a chunk of binary data to handle lifecycle management of data
/// while it is passed between client and HarfBuzz. Blobs are primarily
/// used to create font faces, but also to access font face tables, as well as
/// pass around other binary data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blob {
    pub handle: *mut c::hb_blob_t,
}

impl Blob {
    /// Creates a new "blob" object wrapping data . The mode parameter is used
    /// to negotiate ownership and lifecycle of data .
    ///
    /// Note that this function returns a freshly-allocated empty blob even
    /// if length is zero. This is in contrast to hb_blob_create(), which
    /// returns the singleton empty blob (as returned by hb_blob_get_empty())
    /// if length is zero.
    ///
    /// # Safety
    /// Unless `mode` is [`MemoryMode::Duplicate`], HarfBuzz keeps a pointer
    /// to `data`: it must outlive the blob (and anything created from it).
    pub unsafe fn create(data: &[u8], mode: MemoryMode) -> Result<Blob, Error> {
        let handle = c::hb_blob_create_or_fail(
            data.as_ptr() as *const c_char,
            data.len() as c_uint,
            mode as c::hb_memory_mode_t,
            std::ptr::null_mut(),
            None,
        );
        if handle.is_null() {
            return Err(Error::HarfbuzzFailed);
        }
        Ok(Blob { handle })
    }

    /// Decreases the reference count on blob , and if it reaches zero,
    /// destroys blob , freeing all memory, possibly calling the
    /// destroy-callback the blob was created for if it has not been
    /// called already.
    ///
    /// # Safety
    /// The handle (and its copies) must not be used afterwards unless other
    /// references are still held.
    pub unsafe fn destroy(&mut self) {
        c::hb_blob_destroy(self.handle);
    }

    /// Attaches a user-data key/data pair to the specified blob.
    ///
    /// # Safety
    /// `ptr` must stay valid for as long as it is attached to the blob, and
    /// `destroycb` (if any) must be safe to call with it.
    pub unsafe fn set_user_data<T>(
        self,
        key: *mut c_void,
        ptr: *mut T,
        destroycb: Option<unsafe extern "C" fn(*mut T)>,
        replace: bool,
    ) -> bool {
        // The Zig version wraps the typed callback in a comptime-generated
        // C callback; a `fn(*mut T)` is ABI-identical to `fn(*mut c_void)`.
        let cb: c::hb_destroy_func_t = destroycb.map(|f| {
            std::mem::transmute::<unsafe extern "C" fn(*mut T), unsafe extern "C" fn(*mut c_void)>(
                f,
            )
        });

        c::hb_blob_set_user_data(
            self.handle,
            key as *mut c::hb_user_data_key_t,
            ptr as *mut c_void,
            cb,
            if replace { 1 } else { 0 },
        ) > 0
    }

    /// Fetches the user data associated with the specified key, attached to
    /// the specified font-functions structure.
    pub fn get_user_data<T>(self, key: *mut c_void) -> Option<NonNull<T>> {
        // SAFETY: valid blob handle; the key is only compared by address.
        let opt =
            unsafe { c::hb_blob_get_user_data(self.handle, key as *mut c::hb_user_data_key_t) };
        NonNull::new(opt as *mut T)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob() {
        let data = b"hello";
        // SAFETY: data is 'static.
        let mut blob = unsafe { Blob::create(data, MemoryMode::Readonly) }.unwrap();

        let mut userdata: u8 = 127;
        let mut key: u8 = 0;
        let key_ptr = &mut key as *mut u8 as *mut c_void;
        // SAFETY: userdata outlives the blob.
        assert!(unsafe { blob.set_user_data::<u8>(key_ptr, &mut userdata, None, false) });
        // SAFETY: the pointer is to `userdata`.
        assert!(unsafe { *blob.get_user_data::<u8>(key_ptr).unwrap().as_ptr() } == 127);

        // SAFETY: not used afterwards.
        unsafe { blob.destroy() };
    }
}
