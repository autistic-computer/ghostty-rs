//! Port of `src/stb/main.zig`.
//!
//! STATUS: ported
//!
//! The vendored `nothings/stb` headers (`stb_image.h`,
//! `stb_image_resize.h`) are compiled from `src/stb/stb.c` by `build.rs`,
//! exactly as the Zig build does (`src/build/SharedDeps.zig`). The Zig
//! module `@cImport`s the headers as `stb_c`; here the few functions that
//! ghostty uses are declared by hand.

use std::os::raw::{c_int, c_uchar, c_void};

/// `stbi_uc` from `stb_image.h`.
#[allow(non_camel_case_types)]
pub type stbi_uc = c_uchar;

// We'll just add the exports of the functions or types we actually use
// here, no need to export everything from the C lib if we don't use it.
extern "C" {
    /// `stbi_load_from_memory` from `stb_image.h`.
    pub fn stbi_load_from_memory(
        buffer: *const stbi_uc,
        len: c_int,
        x: *mut c_int,
        y: *mut c_int,
        channels_in_file: *mut c_int,
        desired_channels: c_int,
    ) -> *mut stbi_uc;

    /// `stbi_image_free` from `stb_image.h`.
    pub fn stbi_image_free(retval_from_stbi_load: *mut c_void);

    /// `stbir_resize_uint8` from `stb_image_resize.h`. Returns 0 on
    /// failure.
    pub fn stbir_resize_uint8(
        input_pixels: *const c_uchar,
        input_w: c_int,
        input_h: c_int,
        input_stride_in_bytes: c_int,
        output_pixels: *mut c_uchar,
        output_w: c_int,
        output_h: c_int,
        output_stride_in_bytes: c_int,
        num_channels: c_int,
    ) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not in the Zig source (which has no tests); verifies the vendored C
    // library is compiled and linked.
    #[test]
    fn resize_uint8_links_and_works() {
        let input = [255u8; 4 * 4];
        let mut output = [0u8; 2 * 2];
        // SAFETY: buffers are valid for the given sizes.
        let res = unsafe {
            stbir_resize_uint8(input.as_ptr(), 4, 4, 4, output.as_mut_ptr(), 2, 2, 2, 1)
        };
        assert_ne!(0, res);
        assert_eq!([255u8; 4], output);
    }

    #[test]
    fn load_from_memory_rejects_garbage() {
        let data = [0u8; 16];
        let (mut x, mut y, mut c) = (0, 0, 0);
        // SAFETY: valid buffers and out pointers.
        let ptr = unsafe {
            stbi_load_from_memory(data.as_ptr(), data.len() as c_int, &mut x, &mut y, &mut c, 0)
        };
        assert!(ptr.is_null());
        // SAFETY: freeing null is a no-op (stbi_image_free calls free).
        unsafe { stbi_image_free(ptr as *mut c_void) };
    }
}
