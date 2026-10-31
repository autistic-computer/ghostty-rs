//! Port of `src/highway/src/detect/linux.zig`.
//!
//! STATUS: ported

#![cfg(target_os = "linux")]

/// Reads from the ELF auxiliary vector (set by the kernel at process
/// start).
#[inline]
pub fn getauxval(key: usize) -> usize {
    // SAFETY: getauxval has no preconditions; it returns 0 for unknown keys.
    unsafe { libc::getauxval(key as libc::c_ulong) as usize }
}

/// Wrapper for prctl(2). Returns the raw result reinterpreted as `usize`,
/// with failures encoded as `-errno` like the Zig raw syscall wrapper.
#[inline]
pub fn prctl(option: i32, a2: usize, a3: usize, a4: usize, a5: usize) -> usize {
    // SAFETY: the caller passes a valid prctl option and arguments.
    let ret = unsafe { libc::prctl(option, a2, a3, a4, a5) };
    if ret < 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(1);
        return (-(errno as isize)) as usize;
    }
    ret as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auxv_pagesz() {
        // AT_PAGESZ is always present.
        const AT_PAGESZ: usize = 6;
        assert!(getauxval(AT_PAGESZ) > 0);
    }

    #[test]
    fn prctl_invalid_option_fails() {
        let ret = prctl(-1, 0, 0, 0, 0) as isize;
        assert!(ret < 0);
    }
}
