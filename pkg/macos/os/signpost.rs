//! Port of `src/macos/os/signpost.zig`.
//!
//! STATUS: ported

use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicPtr, Ordering};

use super::c;
use super::log::Log;

/// This should be called once at the start of the program to intialize
/// some required state for signpost logging.
///
/// This is all to workaround a Zig bug:
/// https://github.com/ziglang/zig/issues/24370
///
/// (The Rust port keeps the same approach: it finds the Mach-O header of
/// the image containing this code with `dladdr`.)
pub fn init() {
    if !DSO_HANDLE.load(Ordering::Acquire).is_null() {
        return;
    }

    // Use a function in this image as the symbol to look up.
    let sym = init as fn() as *const c_void;

    // Since __dso_handle is not automatically populated by the linker,
    // we populate it by looking up the function's module address
    // which should be a mach-o header.
    // SAFETY: Dl_info is plain data; dladdr fills it in.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: sym is an address in this image; info is a valid out pointer.
    let result = unsafe { libc::dladdr(sym, &mut info) };
    assert!(result != 0);
    DSO_HANDLE.store(info.dli_fbase, Ordering::Release);
}

/// This should REALLY be an extern var that is populated by the linker,
/// but there is a Zig bug: https://github.com/ziglang/zig/issues/24370
static DSO_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Checks whether signpost logging is enabled for the given log handle.
/// Returns true if signposts will be recorded for this log, false otherwise.
/// This can be used to avoid expensive operations when signpost logging is disabled.
///
/// https://developer.apple.com/documentation/os/os_signpost_enabled?language=objc
pub fn enabled(log: &Log) -> bool {
    // SAFETY: log is live.
    unsafe { c::os_signpost_enabled(log.as_ptr()) }
}

/// Emits a signpost event - a single point in time marker.
/// Events are useful for marking when specific actions occur, such as
/// user interactions, state changes, or other discrete occurrences.
/// The event will appear as a vertical line in Instruments.
///
/// https://developer.apple.com/documentation/os/os_signpost_event_emit?language=objc
pub fn emit_event(log: &Log, id: Id, name: &'static CStr) {
    emit_with_name(log, id, Type::Event, name);
}

/// Marks the beginning of a time interval.
/// Use this with intervalEnd to measure the duration of operations.
/// The same ID must be used for both the begin and end calls.
/// Intervals appear as horizontal bars in Instruments timeline.
///
/// https://developer.apple.com/documentation/os/os_signpost_interval_begin?language=objc
pub fn interval_begin(log: &Log, id: Id, name: &'static CStr) {
    emit_with_name(log, id, Type::IntervalBegin, name);
}

/// Marks the end of a time interval.
/// Must be paired with a prior intervalBegin call using the same ID.
/// The name should match the name used in intervalBegin.
/// Instruments will calculate and display the duration between begin and end.
///
/// https://developer.apple.com/documentation/os/os_signpost_interval_end?language=objc
pub fn interval_end(log: &Log, id: Id, name: &'static CStr) {
    emit_with_name(log, id, Type::IntervalEnd, name);
}

/// The internal function to emit a signpost with a specific name.
///
/// (The name must be a static string, like the Zig comptime name: os_log
/// keeps a pointer to it.)
fn emit_with_name(log: &Log, id: Id, typ: Type, name: &'static CStr) {
    // Init must be called by this point.
    let dso = DSO_HANDLE.load(Ordering::Acquire);
    assert!(!dso.is_null());

    let mut buf = [0u8; 2];
    // SAFETY: all pointers are valid; buf is 2 bytes.
    unsafe {
        c::_os_signpost_emit_with_name_impl(
            dso,
            log.as_ptr(),
            typ as c::os_signpost_type_t,
            id.0,
            name.as_ptr(),
            c"".as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
        );
    }
}

/// https://developer.apple.com/documentation/os/os_signpost_id_t?language=objc
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(pub u64);

impl Id {
    /// OS_SIGNPOST_ID_NULL
    pub const NULL: Id = Id(0);
    /// OS_SIGNPOST_ID_INVALID
    pub const INVALID: Id = Id(0xFFFFFFFFFFFFFFFF);
    /// OS_SIGNPOST_ID_EXCLUSIVE
    pub const EXCLUSIVE: Id = Id(0xEEEEB0B5B2B2EEEE);

    /// Generates a new signpost ID for use with signpost operations.
    /// The ID is unique for the given log handle and can be used to track
    /// asynchronous operations or mark specific points of interest in the code.
    /// Returns a unique signpost ID that can be used with os_signpost functions.
    ///
    /// https://developer.apple.com/documentation/os/os_signpost_id_generate?language=objc
    pub fn generate(log: &Log) -> Id {
        // SAFETY: log is live.
        Id(unsafe { c::os_signpost_id_generate(log.as_ptr()) })
    }

    /// Creates a signpost ID based on a pointer value.
    /// This is useful for tracking operations associated with a specific object
    /// or memory location. The same pointer will always generate the same ID
    /// for a given log handle, allowing correlation of signpost events.
    /// Pass null to get the null signpost ID.
    ///
    /// https://developer.apple.com/documentation/os/os_signpost_id_for_pointer?language=objc
    #[allow(clippy::not_unsafe_ptr_arg_deref)] // only the address is used
    pub fn for_pointer(log: &Log, ptr: *const c_void) -> Id {
        // SAFETY: log is live.
        Id(unsafe { c::os_signpost_id_make_with_pointer(log.as_ptr(), ptr) })
    }
}

/// https://developer.apple.com/documentation/os/ossignposttype?language=objc
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// OS_SIGNPOST_EVENT
    Event = 0,
    /// OS_SIGNPOST_INTERVAL_BEGIN
    IntervalBegin = 1,
    /// OS_SIGNPOST_INTERVAL_END
    IntervalEnd = 2,
}

impl Type {
    /// OS_SIGNPOST_TYPE_MASK
    pub const MASK: u8 = 0x03;
}

/// Special os_log category values that surface in Instruments and other
/// tooling.
pub mod category {
    use std::ffi::CStr;

    /// Points of Interest appear as a dedicated track in Instruments.
    /// Use this for high-level application events that help understand
    /// the flow of your application.
    pub const POINTS_OF_INTEREST: &CStr = c"PointsOfInterest";

    /// Dynamic Tracing category enables runtime-configurable logging.
    /// Signposts in this category can be enabled/disabled dynamically
    /// without recompiling.
    pub const DYNAMIC_TRACING: &CStr = c"DynamicTracing";

    /// Dynamic Stack Tracing category captures call stacks at signpost
    /// events. This provides deeper debugging information but has higher
    /// performance overhead.
    pub const DYNAMIC_STACK_TRACING: &CStr = c"DynamicStackTracing";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_id() {
        // We can't really test the return value because it may return null
        // if signposts are disabled.
        let id = Id::generate(Log::create(c"com.mitchellh.ghostty", c"test"));
        assert!(id != Id::INVALID);
    }

    #[test]
    fn generate_id_for_pointer() {
        let foo: usize = 0x1234;
        let id = Id::for_pointer(
            Log::create(c"com.mitchellh.ghostty", c"test"),
            &foo as *const usize as *const c_void,
        );
        assert!(id != Id::NULL);
    }

    #[test]
    fn enabled_test() {
        let _ = enabled(Log::create(c"com.mitchellh.ghostty", c"test"));
    }

    #[test]
    fn intervals() {
        init();

        let log = Log::create(c"com.mitchellh.ghostty", c"test");

        // Test that we can begin and end an interval
        let id = Id::generate(log);
        interval_begin(log, id, c"Test Interval");
        log.release();
    }
}
