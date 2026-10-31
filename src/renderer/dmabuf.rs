//! Port of `src/renderer/Dmabuf.zig`.
//!
//! STATUS: ported
//!
//! A DMABUF frame produced by exporting a GPU texture.
//! This may be used on apprts like GTK that require us to manually
//! export each frame and import them as textures in their UI scene graphs.
//!
//! Note that DMABUFs are independent of the graphics API used:
//! the OpenGL renderer allocates them with GBM, and Vulkan can
//! export them with a KHR external memory implementation. Therefore
//! this struct has to be placed parallel to the renderer
//! implementations.

use std::os::raw::c_int;

/// The maximum number of planes in a DMABUF that we support.
/// This matches the maximum found in apprts such as GTK.
pub const MAX_PLANES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dmabuf {
    /// Width of the texture in device pixels.
    pub width: u32,

    /// Height of the texture in device pixels.
    pub height: u32,

    /// DRM fourcc of the pixel format.
    /// Ghostty renders RGBA/BGRA8 premultiplied.
    pub fourcc: u32,

    /// DRM modifier of the format.
    pub modifier: u64,

    /// Whether the data is premultiplied.
    /// Ghostty's GL renderers output premultiplied alpha.
    pub premultiplied: bool,

    /// The DMABUF planes for a presented frame. The DMABUF owns the fds
    /// and must either call `deinit` manually, or pass them to an apprt
    /// that consumes them.
    pub planes: Planes,
}

/// Zig: `error{BadDmabuf}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanesError {
    BadDmabuf,
}

impl std::fmt::Display for PlanesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for PlanesError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Planes {
    /// Number of planes. Valid planes are `planes[0..count]`.
    pub count: u8,

    /// File descriptor for each plane.
    pub fds: [c_int; MAX_PLANES],

    /// Offset into the DMABUF where each plane starts, in bytes.
    pub offsets: [c_int; MAX_PLANES],

    /// Strides of each plane, in bytes.
    pub strides: [c_int; MAX_PLANES],
}

impl Planes {
    /// Planes with the given count and the Zig field defaults (all fds
    /// `-1`, all offsets/strides `0`).
    pub const fn new(count: u8) -> Planes {
        Planes {
            count,
            fds: [-1; MAX_PLANES],
            offsets: [0; MAX_PLANES],
            strides: [0; MAX_PLANES],
        }
    }

    /// Close all valid fds.
    pub fn deinit(self) {
        for &fd in &self.fds[..self.count as usize] {
            if fd >= 0 {
                // SAFETY: the planes own their fds.
                unsafe { libc::close(fd) };
            }
        }
    }

    /// Validate the current planes. If any plane failed to export
    /// and has an invalid FD, we close all the known valid FDs
    /// and bail.
    pub fn validate(self) -> Result<(), PlanesError> {
        let mut n_valid: usize = 0;
        while n_valid < self.count as usize {
            if self.fds[n_valid] < 0 {
                for &bad in &self.fds[..n_valid] {
                    // SAFETY: the planes own their fds.
                    unsafe { libc::close(bad) };
                }
                return Err(PlanesError::BadDmabuf);
            }
            n_valid += 1;
        }
        Ok(())
    }
}

impl Dmabuf {
    pub fn deinit(self) {
        self.planes.deinit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe() -> [c_int; 2] {
        let mut fds = [0 as c_int; 2];
        // SAFETY: fds has room for two descriptors.
        assert_eq!(0, unsafe { libc::pipe(fds.as_mut_ptr()) });
        fds
    }

    #[test]
    fn planes_validate_and_deinit() {
        let fds = pipe();
        let mut planes = Planes::new(2);
        planes.fds[0] = fds[0];
        planes.fds[1] = fds[1];
        assert_eq!(Ok(()), planes.validate());
        // Closes both ends of the pipe.
        planes.deinit();
    }

    #[test]
    fn planes_validate_closes_on_bad_fd() {
        let fds = pipe();
        let mut planes = Planes::new(3);
        planes.fds[0] = fds[0];
        planes.fds[2] = fds[1];
        assert_eq!(Err(PlanesError::BadDmabuf), planes.validate());
        // Only the planes before the bad one were closed (fds[0]); the
        // fd after the bad plane is still ours.
        // SAFETY: we own fds[1].
        assert_eq!(0, unsafe { libc::close(fds[1]) });
    }
}
