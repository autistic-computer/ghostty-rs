//! Port of `src/quirks.zig`.
//!
//! STATUS: ported
//!
//! Inspired by WebKit's quirks.cpp[1], this file centralizes all our
//! sad environment-specific hacks that we have to do to make things work.
//! This is a last resort; if we can find a general solution to a problem,
//! we of course prefer that, but sometimes other software, fonts, etc. are
//! just broken or weird and we have to work around it.
//!
//! [1]: https://github.com/WebKit/WebKit/blob/main/Source/WebCore/page/Quirks.cpp

/// If true, the default font features should be disabled for the given face.
///
/// The Zig version takes a `*const font.Face`. The face is unused, so this
/// port is generic over the face type to avoid tying this module to the
/// (backend-specific) font face type.
pub fn disable_default_font_features<F: ?Sized>(face: &F) -> bool {
    let _ = face;

    // This function used to do something, but we integrated the logic
    // we checked for directly into our shaping algorithm. It's likely
    // there are other broken fonts for other reasons so I'm keeping this
    // around so its easy to add more checks in the future.
    false

    // var buf: [64]u8 = undefined;
    // const name = face.name(&buf) catch |err| switch (err) {
    //     // If the name doesn't fit in buf we know this will be false
    //     // because we have no quirks fonts that are longer than buf!
    //     error.OutOfMemory => return false,
    // };
}

/// We use our own assert function instead of `std.debug.assert`.
///
/// The only difference between this and the one in
/// the stdlib is that this version is marked inline.
///
/// The reason for this is that, despite the promises of the doc comment
/// on the stdlib function, the function call to `std.debug.assert` isn't
/// always optimized away in `ReleaseFast` mode, at least in Zig 0.15.2.
///
/// In the majority of places, the overhead from calling an empty function
/// is negligible, but we have some asserts inside tight loops and hotpaths
/// that cause significant overhead (as much as 15-20%) when they don't get
/// optimized out.
///
/// In debug builds (`debug_assertions`) this is a regular checked assert.
/// In release builds a failed assertion is undefined behavior, exactly
/// like Zig's `unreachable` in `ReleaseFast`, which lets the optimizer
/// use the asserted condition.
#[inline(always)]
#[track_caller]
pub fn inline_assert(ok: bool) {
    if cfg!(debug_assertions) {
        assert!(ok);
    } else if !ok {
        // SAFETY: callers guarantee the condition holds; this mirrors
        // Zig's `if (!ok) unreachable;` in optimized builds.
        unsafe { std::hint::unreachable_unchecked() }
    }
}
