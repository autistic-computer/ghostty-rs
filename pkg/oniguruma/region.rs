//! Port of `pkg/oniguruma/region.zig`.
//!
//! STATUS: ported

use std::os::raw::c_int;

use super::c;

/// An `OnigRegion` owned by Rust (the Zig struct is an `extern struct` with
/// the same layout). The arrays it points to are allocated by Oniguruma and
/// freed on drop or by [`Region::deinit`].
#[repr(C)]
#[derive(Debug)]
pub struct Region {
    pub allocated: c_int,
    pub num_regs: c_int,
    pub beg: *mut c_int,
    pub end: *mut c_int,
    pub history_root: *mut c::OnigCaptureTreeNode, // TODO: convert to Zig
}

const _: () = assert!(std::mem::size_of::<Region>() == std::mem::size_of::<c::OnigRegion>());

// SAFETY: the region's arrays are exclusively owned.
unsafe impl Send for Region {}

impl Default for Region {
    fn default() -> Region {
        Region {
            allocated: 0,
            num_regs: 0,
            beg: std::ptr::null_mut(),
            end: std::ptr::null_mut(),
            history_root: std::ptr::null_mut(),
        }
    }
}

impl Region {
    /// Frees the match arrays. Safe to call more than once (it is also
    /// called on drop); the region is empty afterwards.
    pub fn deinit(&mut self) {
        // We never free ourself because allocation of Region in the Zig
        // bindings is handled by the Zig program.
        // SAFETY: Region has the layout of OnigRegion.
        unsafe { c::onig_region_free(self as *mut Region as *mut c::OnigRegion, 0) };
        self.num_regs = 0;
        self.beg = std::ptr::null_mut();
        self.end = std::ptr::null_mut();
    }

    /// Count the number of matches
    pub fn count(&self) -> usize {
        self.num_regs as usize
    }

    /// Iterate over the matched ranges.
    pub fn iterator(&self) -> Iterator<'_> {
        Iterator { region: self, i: 0 }
    }

    pub fn starts(&self) -> &[c_int] {
        if self.num_regs == 0 {
            return &[];
        }
        // SAFETY: beg holds num_regs entries.
        unsafe { std::slice::from_raw_parts(self.beg, self.num_regs as usize) }
    }

    pub fn ends(&self) -> &[c_int] {
        if self.num_regs == 0 {
            return &[];
        }
        // SAFETY: end holds num_regs entries.
        unsafe { std::slice::from_raw_parts(self.end, self.num_regs as usize) }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        self.deinit();
    }
}

/// Zig: `Region.Iterator`.
pub struct Iterator<'a> {
    region: &'a Region,
    i: usize,
}

impl std::iter::Iterator for Iterator<'_> {
    type Item = [usize; 2];

    /// The next range
    fn next(&mut self) -> Option<[usize; 2]> {
        if self.i >= self.region.count() {
            return None;
        }
        let i = self.i;
        self.i += 1;
        Some([
            self.region.starts()[i] as usize,
            self.region.ends()[i] as usize,
        ])
    }
}
