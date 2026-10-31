//! Port of `src/lib/allocator.zig`.
//!
//! STATUS: ported
//!
//! The Rust port uses the global allocator internally (see PORTING.md);
//! this module only exists for the C API boundary, where embedders may pass
//! a `GhosttyAllocator`. [`ZigAllocator`] is the Rust counterpart of Zig's
//! `std.mem.Allocator` vtable interface, so C allocators can be wrapped as
//! Rust allocators and vice versa. Alignments are passed as log2 byte
//! units, exactly like Zig's `std.mem.Alignment`.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "allocator/wasm.rs"]
pub mod wasm;
// @generated-mods-end

use std::ffi::c_void;

/// The Rust counterpart of Zig's `std.mem.Allocator` interface
/// (Zig: `ZigVTable`). Implementors must follow the Zig allocator
/// contract: `alloc` returns null on failure, `resize` resizes in place,
/// `remap` may move, `free` releases memory previously returned.
///
/// `alignment` is the log2 of the byte alignment. `ret_addr` is a return
/// address hint used only for diagnostics (may be 0).
pub trait ZigAllocator {
    fn alloc(&self, len: usize, alignment: u8, ret_addr: usize) -> *mut u8;
    fn resize(
        &self,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        new_len: usize,
        ret_addr: usize,
    ) -> bool;
    fn remap(
        &self,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        new_len: usize,
        ret_addr: usize,
    ) -> *mut u8;
    fn free(&self, memory: *mut u8, memory_len: usize, alignment: u8, ret_addr: usize);
}

/// The VTable required by the C interface.
/// C: GhosttyAllocatorVtable
#[repr(C)]
pub struct VTable {
    pub alloc:
        unsafe extern "C" fn(*mut c_void, len: usize, alignment: u8, ret_addr: usize) -> *mut u8,
    pub resize: unsafe extern "C" fn(
        *mut c_void,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        new_len: usize,
        ret_addr: usize,
    ) -> bool,
    pub remap: unsafe extern "C" fn(
        *mut c_void,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        new_len: usize,
        ret_addr: usize,
    ) -> *mut u8,
    pub free: unsafe extern "C" fn(
        *mut c_void,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        ret_addr: usize,
    ),
}

/// The Allocator interface for custom memory allocation strategies
/// within C libghostty APIs.
///
/// This -- purposely -- matches the Zig allocator interface. We do this
/// for two reasons: (1) Zig's allocator interface is well proven in
/// the real world to be flexible and useful, and (2) it allows us to
/// easily convert C allocators to Rust allocators and vice versa.
///
/// C: GhosttyAllocator
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Allocator {
    pub ctx: *mut c_void,
    pub vtable: *const VTable,
}

// SAFETY: the C API requires allocators to be usable from any thread.
unsafe impl Send for Allocator {}
// SAFETY: see above.
unsafe impl Sync for Allocator {}

impl Allocator {
    /// Create a C allocator from a Rust allocator. This requires that
    /// the Rust allocator be pointer-stable for the lifetime of the
    /// C allocator (hence the `&'static`-or-outliving reference to the
    /// trait object reference, like Zig's `*const std.mem.Allocator`).
    pub const fn from_zig(zig_alloc: &&dyn ZigAllocator) -> Allocator {
        Allocator {
            ctx: zig_alloc as *const &dyn ZigAllocator as *mut c_void,
            vtable: &ZIG_ALLOCATOR_VTABLE,
        }
    }

    /// Create a Rust allocator from this C allocator.
    pub fn zig(&self) -> CAllocator<'_> {
        CAllocator { c: self }
    }
}

/// A Rust [`ZigAllocator`] that forwards to a C [`Allocator`]
/// (Zig: `Allocator.zig_vtable`).
#[derive(Clone, Copy)]
pub struct CAllocator<'a> {
    c: &'a Allocator,
}

impl ZigAllocator for CAllocator<'_> {
    fn alloc(&self, len: usize, alignment: u8, ra: usize) -> *mut u8 {
        // SAFETY: the C allocator's vtable is valid by construction.
        unsafe { ((*self.c.vtable).alloc)(self.c.ctx, len, alignment, ra) }
    }

    fn resize(
        &self,
        old_mem: *mut u8,
        old_len: usize,
        alignment: u8,
        new_len: usize,
        ra: usize,
    ) -> bool {
        // SAFETY: see `alloc`.
        unsafe { ((*self.c.vtable).resize)(self.c.ctx, old_mem, old_len, alignment, new_len, ra) }
    }

    fn remap(
        &self,
        old_mem: *mut u8,
        old_len: usize,
        alignment: u8,
        new_len: usize,
        ra: usize,
    ) -> *mut u8 {
        // SAFETY: see `alloc`.
        unsafe { ((*self.c.vtable).remap)(self.c.ctx, old_mem, old_len, alignment, new_len, ra) }
    }

    fn free(&self, old_mem: *mut u8, old_len: usize, alignment: u8, ra: usize) {
        // SAFETY: see `alloc`.
        unsafe { ((*self.c.vtable).free)(self.c.ctx, old_mem, old_len, alignment, ra) }
    }
}

/// An allocator implementation that wraps a Rust allocator so that
/// it can be exposed to C.
static ZIG_ALLOCATOR_VTABLE: VTable = VTable {
    alloc: zig_allocator_alloc,
    resize: zig_allocator_resize,
    remap: zig_allocator_remap,
    free: zig_allocator_free,
};

unsafe fn ctx_allocator<'a>(ctx: *mut c_void) -> &'a dyn ZigAllocator {
    // SAFETY: `ctx` was created by `Allocator::from_zig` from a
    // `&&dyn ZigAllocator` that outlives the C allocator.
    unsafe { *(ctx as *const &dyn ZigAllocator) }
}

unsafe extern "C" fn zig_allocator_alloc(
    ctx: *mut c_void,
    len: usize,
    alignment: u8,
    ra: usize,
) -> *mut u8 {
    unsafe { ctx_allocator(ctx) }.alloc(len, alignment, ra)
}

unsafe extern "C" fn zig_allocator_resize(
    ctx: *mut c_void,
    memory: *mut u8,
    memory_len: usize,
    alignment: u8,
    new_len: usize,
    ra: usize,
) -> bool {
    unsafe { ctx_allocator(ctx) }.resize(memory, memory_len, alignment, new_len, ra)
}

unsafe extern "C" fn zig_allocator_remap(
    ctx: *mut c_void,
    memory: *mut u8,
    memory_len: usize,
    alignment: u8,
    new_len: usize,
    ra: usize,
) -> *mut u8 {
    unsafe { ctx_allocator(ctx) }.remap(memory, memory_len, alignment, new_len, ra)
}

unsafe extern "C" fn zig_allocator_free(
    ctx: *mut c_void,
    memory: *mut u8,
    memory_len: usize,
    alignment: u8,
    ra: usize,
) {
    unsafe { ctx_allocator(ctx) }.free(memory, memory_len, alignment, ra)
}

/// The libc allocator (Zig: `std.heap.c_allocator`). Supports any
/// power-of-two alignment through `posix_memalign` on POSIX.
#[derive(Debug, Clone, Copy, Default)]
pub struct LibcAllocator;

impl LibcAllocator {
    fn needs_aligned(alignment: u8) -> bool {
        (1usize << alignment) > std::mem::align_of::<libc::max_align_t>()
    }
}

impl ZigAllocator for LibcAllocator {
    fn alloc(&self, len: usize, alignment: u8, _ra: usize) -> *mut u8 {
        if Self::needs_aligned(alignment) {
            #[cfg(unix)]
            {
                let mut ptr: *mut c_void = std::ptr::null_mut();
                let align = (1usize << alignment).max(std::mem::size_of::<*const ()>());
                // SAFETY: valid out pointer and power-of-two alignment.
                let rc = unsafe { libc::posix_memalign(&mut ptr, align, len) };
                return if rc == 0 {
                    ptr as *mut u8
                } else {
                    std::ptr::null_mut()
                };
            }
            #[cfg(not(unix))]
            return std::ptr::null_mut();
        }
        // SAFETY: malloc has no preconditions.
        unsafe { libc::malloc(len) as *mut u8 }
    }

    fn resize(
        &self,
        memory: *mut u8,
        memory_len: usize,
        _alignment: u8,
        new_len: usize,
        _ra: usize,
    ) -> bool {
        if new_len <= memory_len {
            return true;
        }
        #[cfg(target_os = "linux")]
        {
            // SAFETY: memory came from malloc/posix_memalign.
            let full = unsafe { libc::malloc_usable_size(memory as *mut c_void) };
            return new_len <= full;
        }
        #[allow(unreachable_code)]
        false
    }

    fn remap(
        &self,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        new_len: usize,
        ra: usize,
    ) -> *mut u8 {
        // realloc would potentially return a new allocation that does not
        // respect the original alignment.
        if self.resize(memory, memory_len, alignment, new_len, ra) {
            memory
        } else {
            std::ptr::null_mut()
        }
    }

    fn free(&self, memory: *mut u8, _memory_len: usize, _alignment: u8, _ra: usize) {
        // SAFETY: memory came from malloc/posix_memalign.
        unsafe { libc::free(memory as *mut c_void) }
    }
}

/// The global Rust allocator exposed through the [`ZigAllocator`]
/// interface. `resize` only succeeds in place when the size does not grow.
#[derive(Debug, Clone, Copy, Default)]
pub struct GlobalAllocator;

impl GlobalAllocator {
    fn layout(len: usize, alignment: u8) -> Option<std::alloc::Layout> {
        std::alloc::Layout::from_size_align(len.max(1), 1usize << alignment).ok()
    }
}

impl ZigAllocator for GlobalAllocator {
    fn alloc(&self, len: usize, alignment: u8, _ra: usize) -> *mut u8 {
        match Self::layout(len, alignment) {
            // SAFETY: layout has non-zero size.
            Some(layout) => unsafe { std::alloc::alloc(layout) },
            None => std::ptr::null_mut(),
        }
    }

    fn resize(
        &self,
        _memory: *mut u8,
        memory_len: usize,
        _alignment: u8,
        new_len: usize,
        _ra: usize,
    ) -> bool {
        // The global allocator has no in-place resize; shrinking is
        // allowed only when it keeps the same layout size class, which we
        // can't know, so only identical sizes succeed.
        new_len == memory_len
    }

    fn remap(
        &self,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        new_len: usize,
        _ra: usize,
    ) -> *mut u8 {
        match Self::layout(memory_len, alignment) {
            // SAFETY: memory was allocated with this layout.
            Some(layout) => unsafe { std::alloc::realloc(memory, layout, new_len.max(1)) },
            None => std::ptr::null_mut(),
        }
    }

    fn free(&self, memory: *mut u8, memory_len: usize, alignment: u8, _ra: usize) {
        if let Some(layout) = Self::layout(memory_len, alignment) {
            // SAFETY: memory was allocated with this layout.
            unsafe { std::alloc::dealloc(memory, layout) }
        }
    }
}

static LIBC_ALLOCATOR: LibcAllocator = LibcAllocator;

/// A `'static` trait object reference that can live in a `static`.
struct StaticDyn(&'static dyn ZigAllocator);
// SAFETY: only wraps stateless allocators.
unsafe impl Sync for StaticDyn {}

static LIBC_ALLOCATOR_DYN: StaticDyn = StaticDyn(&LIBC_ALLOCATOR);

/// libc Allocator, requires linking libc
pub static C_ALLOCATOR: Allocator = Allocator::from_zig(&LIBC_ALLOCATOR_DYN.0);

/// The allocator returned by [`default`].
#[derive(Clone, Copy)]
pub enum DefaultAllocator<'a> {
    /// A caller-provided C allocator.
    C(CAllocator<'a>),
    /// libc. We prefer libc if we have it because its generally fast but
    /// also lets the embedder easily override malloc/free with custom
    /// allocators like mimalloc or something.
    Libc,
}

impl ZigAllocator for DefaultAllocator<'_> {
    fn alloc(&self, len: usize, alignment: u8, ra: usize) -> *mut u8 {
        match self {
            DefaultAllocator::C(c) => c.alloc(len, alignment, ra),
            DefaultAllocator::Libc => LIBC_ALLOCATOR.alloc(len, alignment, ra),
        }
    }

    fn resize(
        &self,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        new_len: usize,
        ra: usize,
    ) -> bool {
        match self {
            DefaultAllocator::C(c) => c.resize(memory, memory_len, alignment, new_len, ra),
            DefaultAllocator::Libc => {
                LIBC_ALLOCATOR.resize(memory, memory_len, alignment, new_len, ra)
            }
        }
    }

    fn remap(
        &self,
        memory: *mut u8,
        memory_len: usize,
        alignment: u8,
        new_len: usize,
        ra: usize,
    ) -> *mut u8 {
        match self {
            DefaultAllocator::C(c) => c.remap(memory, memory_len, alignment, new_len, ra),
            DefaultAllocator::Libc => {
                LIBC_ALLOCATOR.remap(memory, memory_len, alignment, new_len, ra)
            }
        }
    }

    fn free(&self, memory: *mut u8, memory_len: usize, alignment: u8, ra: usize) {
        match self {
            DefaultAllocator::C(c) => c.free(memory, memory_len, alignment, ra),
            DefaultAllocator::Libc => LIBC_ALLOCATOR.free(memory, memory_len, alignment, ra),
        }
    }
}

/// Returns an allocator to use for the given possibly-null C allocator,
/// ensuring some allocator is always returned.
///
/// Zig additionally picks the leak-checking test allocator in tests, the
/// Wasm allocator on Wasm and a failing allocator on freestanding targets;
/// the Rust port always links a libc-compatible allocator (`libc` on Wasm
/// is provided by the Rust toolchain's `wasi`/`dlmalloc` support).
pub fn default(c_alloc: Option<&Allocator>) -> DefaultAllocator<'_> {
    // If we're given an allocator, use it.
    if let Some(c_alloc) = c_alloc {
        return DefaultAllocator::C(c_alloc.zig());
    }
    DefaultAllocator::Libc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn c_allocator() {
        let alloc = C_ALLOCATOR.zig();
        let str = alloc.alloc(10, 0, 0);
        assert!(!str.is_null());
        // SAFETY: 10 bytes were allocated.
        unsafe { std::ptr::write_bytes(str, 7, 10) };
        alloc.free(str, 10, 0, 0);
    }

    /// Port of Zig's `std.heap.FixedBufferAllocator` (bump allocator).
    struct FixedBufferAllocator {
        buf: *mut u8,
        len: usize,
        end: Cell<usize>,
    }

    impl ZigAllocator for FixedBufferAllocator {
        fn alloc(&self, len: usize, alignment: u8, _ra: usize) -> *mut u8 {
            let align = 1usize << alignment;
            let base = self.buf as usize + self.end.get();
            let aligned = (base + align - 1) & !(align - 1);
            let start = aligned - self.buf as usize;
            if start + len > self.len {
                return std::ptr::null_mut();
            }
            self.end.set(start + len);
            aligned as *mut u8
        }

        fn resize(&self, _m: *mut u8, old: usize, _a: u8, new: usize, _ra: usize) -> bool {
            new <= old
        }

        fn remap(&self, m: *mut u8, old: usize, a: u8, new: usize, ra: usize) -> *mut u8 {
            if self.resize(m, old, a, new, ra) {
                m
            } else {
                std::ptr::null_mut()
            }
        }

        fn free(&self, m: *mut u8, len: usize, _a: u8, _ra: usize) {
            // Only the last allocation can be freed.
            if m as usize + len == self.buf as usize + self.end.get() {
                self.end.set(self.end.get() - len);
            }
        }
    }

    #[test]
    fn fba_allocator() {
        let mut buf = [0u8; 1024];
        let fba = FixedBufferAllocator {
            buf: buf.as_mut_ptr(),
            len: buf.len(),
            end: Cell::new(0),
        };
        let zig_alloc: &dyn ZigAllocator = &fba;

        // Convert the Rust allocator to a C interface
        let c_alloc = Allocator::from_zig(&zig_alloc);

        // Convert back to Rust so we can test it.
        let alloc = c_alloc.zig();
        let str = alloc.alloc(10, 0, 0);
        assert!(!str.is_null());
        assert_eq!(10, fba.end.get());
        alloc.free(str, 10, 0, 0);
        assert_eq!(0, fba.end.get());
    }

    #[test]
    fn default_uses_given_allocator() {
        assert!(matches!(default(None), DefaultAllocator::Libc));
        assert!(matches!(
            default(Some(&C_ALLOCATOR)),
            DefaultAllocator::C(_)
        ));
        let p = default(None).alloc(32, 5, 0);
        assert_eq!(0, p as usize % 32);
        default(None).free(p, 32, 5, 0);
    }
}
