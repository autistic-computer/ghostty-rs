//! Port of `src/lib/windows_dll.zig`.
//!
//! STATUS: ported (Windows-only; best effort, not compiled on Linux)
//!
//! Global constructor initialization for the libghostty-vt Windows DLL.
//!
//! Zig's `std.start` exports a `_DllMainCRTStartup` for any dynamic library
//! that returns TRUE without doing any C runtime work, and because it is the
//! image's entry point the real CRT bootstrap never runs: on MSVC that is
//! `__scrt_dllmain_crt_initialize`, and on MinGW it is `dllcrt2.obj`, which
//! is linked but never reached.
//!
//! What we have to do by hand is run the C++ global constructors. Note that
//! we do *not* have to bootstrap the CRT itself: libghostty-vt links the
//! DLL-based CRT, and those DLLs initialize their own state from their own
//! `DllMain`.
//!
//! Running the initializers matters because we build the vendored simdutf
//! with `SIMDUTF_NO_LIBCXX`, which makes simdutf hold its active-kernel
//! pointer and every kernel singleton in translation-unit-scope statics.
//! Left unconstructed those are all null, so the first call into simdutf
//! makes a virtual call through a null `implementation*`.
//!
//! This is all a workaround for the Zig toolchain. A Rust `cdylib` uses the
//! platform CRT entry point, which already runs these initializers, so the
//! port is only needed if the library is linked with a custom entry point
//! that bypasses the CRT. Remove it when no longer necessary.

#![cfg(windows)]

use crate::os::windows::{BOOL, DWORD, HINSTANCE, LPVOID, TRUE};

const DLL_PROCESS_ATTACH: DWORD = 1;

/// A global constructor, as the linker collects them.
type Initializer = Option<unsafe extern "C" fn()>;

/// MSVC collects global constructors into `.CRT$XCU`. The linker
/// concatenates `.CRT$*` sections in alphabetical order, so a marker in
/// `.CRT$XCA` and one in `.CRT$XCZ` bracket every initializer in the image.
/// This is how the CRT's own `_initterm` finds them.
#[cfg(target_env = "msvc")]
#[used]
#[no_mangle]
#[link_section = ".CRT$XCA"]
pub static ghostty_crt_xc_a: Initializer = None;

#[cfg(target_env = "msvc")]
#[used]
#[no_mangle]
#[link_section = ".CRT$XCZ"]
pub static ghostty_crt_xc_z: Initializer = None;

// MinGW collects global constructors into `__CTOR_LIST__` instead: a
// sentinel of -1, then the initializers, then a null terminator. This is
// the list that `dllcrt2.obj` would have walked.
#[cfg(not(target_env = "msvc"))]
extern "C" {
    static __CTOR_LIST__: Initializer;
}

#[allow(non_snake_case)]
pub extern "system" fn DllMain(hinstDLL: HINSTANCE, fdwReason: DWORD, lpReserved: LPVOID) -> BOOL {
    let _ = hinstDLL;
    let _ = lpReserved;

    if fdwReason == DLL_PROCESS_ATTACH {
        run_global_constructors();
    }

    TRUE
}

fn run_global_constructors() {
    #[cfg(not(target_env = "msvc"))]
    {
        // We walk `__CTOR_LIST__` ourselves rather than calling `__main`,
        // which is what an executable would use. `__main` also performs
        // atexit and exception-handling registration that is not safe from
        // `DllMain`; doing it there corrupts the heap before the loader
        // even returns.
        // SAFETY: the linker guarantees the list layout described above.
        unsafe {
            let list = std::ptr::addr_of!(__CTOR_LIST__);
            let mut len = 0usize;
            while (*list.add(len + 1)).is_some() {
                len += 1;
            }

            // MinGW runs these in reverse list order.
            let mut i = len;
            while i >= 1 {
                if let Some(f) = *list.add(i) {
                    f();
                }
                i -= 1;
            }
        }
    }

    #[cfg(target_env = "msvc")]
    {
        // SAFETY: the markers bracket the `.CRT$XCU` initializer array.
        unsafe {
            let start = std::ptr::addr_of!(ghostty_crt_xc_a);
            let end = std::ptr::addr_of!(ghostty_crt_xc_z);
            let len = (end as usize - start as usize) / std::mem::size_of::<Initializer>();
            for i in 0..len {
                if let Some(f) = *start.add(i) {
                    f();
                }
            }
        }
    }
}
