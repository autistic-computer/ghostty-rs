//! Port of `src/lib/types.zig`.
//!
//! STATUS: ported

/// Storage for empty borrowed strings. See [`String::init`].
static EMPTY: [u8; 1] = [0];

/// A borrowed, C ABI compatible byte string.
///
/// C: GhosttyString
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct String {
    pub ptr: *const u8,
    pub len: usize,
}

impl String {
    pub fn init(zig: &[u8]) -> String {
        String {
            // Borrowed strings keep a non-null pointer, but it must point
            // to real storage: empty Rust slices use dangling pointers
            // that foreign runtimes reject even without dereferencing.
            ptr: if zig.is_empty() {
                EMPTY.as_ptr()
            } else {
                zig.as_ptr()
            },
            len: zig.len(),
        }
    }

    /// The pointer used for empty strings (the Rust counterpart of Zig's
    /// `""` literal storage).
    pub fn empty_ptr() -> *const u8 {
        EMPTY.as_ptr()
    }
}

/// A caller-provided, C ABI compatible output buffer.
///
/// C: GhosttyBuffer
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Buffer {
    pub ptr: *mut u8,
    pub cap: usize,
    pub len: usize,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer {
            ptr: std::ptr::null_mut(),
            cap: 0,
            len: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_init_empty_output() {
        let empty: Vec<u8> = Vec::new();
        let str = String::init(&empty);
        assert_eq!(0usize, str.len);
        // Empty borrowed strings use the same real storage as an empty
        // literal, never Rust's dangling zero-length allocation pointer.
        assert_eq!(String::empty_ptr(), str.ptr);
        assert_ne!(empty.as_ptr(), str.ptr);
    }
}
