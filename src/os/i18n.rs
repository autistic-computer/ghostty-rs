//! Port of `src/os/i18n.zig`.
//!
//! STATUS: ported
//!
//! gettext integration. On Linux the functions come from glibc's built-in
//! libintl via FFI; on macOS from the libintl Ghostty builds from source.

use std::collections::HashSet;
use std::ffi::{c_char, CStr, CString};
use std::fmt;
use std::sync::LazyLock;

use super::i18n_locales::LOCALES;

const BUILD_I18N: bool = crate::build_config::I18N;

/// `build_config.bundle_id`, NUL-terminated for gettext.
const BUNDLE_ID_Z: [u8; crate::build_config::BUNDLE_ID.len() + 1] = {
    let src = crate::build_config::BUNDLE_ID.as_bytes();
    let mut out = [0u8; crate::build_config::BUNDLE_ID.len() + 1];
    let mut i = 0;
    while i < src.len() {
        out[i] = src[i];
        i += 1;
    }
    out
};
const BUNDLE_ID: &CStr = match CStr::from_bytes_with_nul(&BUNDLE_ID_Z) {
    Ok(v) => v,
    Err(_) => panic!("bundle id must not contain NUL"),
};

/// Set for faster membership lookup of locales.
pub static LOCALES_MAP: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| LOCALES.iter().copied().collect());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    InvalidResourcesDir,
    OutOfMemory,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for InitError {}

/// Initialize i18n support for the application. This should be
/// called automatically by the global state initialization
/// in global.zig.
///
/// This calls `bindtextdomain` for gettext with the proper directory
/// of translations. This does NOT call `textdomain` as we don't
/// want to set the domain for the entire application since this is also
/// used by libghostty.
pub fn init(resources_dir: &str) -> Result<(), InitError> {
    if !BUILD_I18N {
        return Ok(());
    }

    // i18n is unsupported on Windows
    #[cfg(not(windows))]
    {
        // Our resources dir is always nested below the share dir that
        // is standard for translations.
        let share_dir = std::path::Path::new(resources_dir)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or(InitError::InvalidResourcesDir)?;

        // Build our locale path
        let path = format!("{}/locale", share_dir.display());
        let path = CString::new(path).map_err(|_| InitError::OutOfMemory)?;

        // Bind our bundle ID to the given locale path
        log::debug!(target: "i18n", "binding domain={:?} path={:?}", BUNDLE_ID, path);
        // SAFETY: both arguments are valid NUL-terminated strings.
        let rc = unsafe { ffi::bindtextdomain(BUNDLE_ID.as_ptr(), path.as_ptr()) };
        if rc.is_null() {
            return Err(InitError::OutOfMemory);
        }
    }

    Ok(())
}

/// Set the global gettext domain to our bundle ID, allowing unqualified
/// `gettext` (`_`) calls to look up translations for our application.
///
/// This should only be called for apprts that are fully owning the
/// Ghostty application. This should not be called for libghostty users.
pub fn init_global_domain() -> Result<(), InitError> {
    if !BUILD_I18N {
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        // SAFETY: valid NUL-terminated string.
        if unsafe { ffi::textdomain(BUNDLE_ID.as_ptr()) }.is_null() {
            return Err(InitError::OutOfMemory);
        }
    }
    Ok(())
}

/// Translate a message for the Ghostty domain (Zig: `_`).
///
/// The returned string is either `msgid` itself or a translation owned by
/// gettext, which lives for the rest of the program (so it outlives `'a`).
pub fn gettext<'a>(msgid: &'a CStr) -> &'a CStr {
    if !BUILD_I18N {
        return msgid;
    }
    #[cfg(not(windows))]
    {
        // SAFETY: valid NUL-terminated strings; dgettext returns either
        // msgid or a pointer into its static catalog.
        return unsafe { CStr::from_ptr(ffi::dgettext(BUNDLE_ID.as_ptr(), msgid.as_ptr())) };
    }
    #[allow(unreachable_code)]
    msgid
}

/// Translate at compile time: returns the msgid untranslated. This still
/// allows the string to be marked for translation while remaining usable
/// in `const` contexts (Zig: `_` called at comptime).
pub const fn gettext_const(msgid: &'static CStr) -> &'static CStr {
    msgid
}

/// Mark a string for translation without translating it immediately.
#[allow(non_snake_case)]
pub const fn N_(msgid: &'static CStr) -> &'static CStr {
    msgid
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSpaceLeft;

impl fmt::Display for NoSpaceLeft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NoSpaceLeft")
    }
}

impl std::error::Error for NoSpaceLeft {}

fn copy_nul<'a>(buf: &'a mut [u8], s: &[u8]) -> Result<&'a str, NoSpaceLeft> {
    if buf.len() < s.len() + 1 {
        return Err(NoSpaceLeft);
    }
    buf[..s.len()].copy_from_slice(s);
    buf[s.len()] = 0;
    std::str::from_utf8(&buf[..s.len()]).map_err(|_| NoSpaceLeft)
}

/// Canonicalize a locale name from a platform-specific value to
/// a POSIX-compliant value. This is a thin layer over the unexported
/// gnulib-lib function in gettext that does this already.
///
/// The gnulib-lib function modifies the buffer in place but has
/// zero bounds checking, so we do a bit extra to ensure we don't
/// overflow the buffer. This is likely slightly more expensive but
/// this isn't a hot path so it should be fine.
///
/// The buffer must be at least 16 bytes long. This ensures we can
/// fit the longest possible hardcoded locale name. Additionally,
/// it should be at least as long as locale in case the locale
/// is unchanged. The result is NUL-terminated in `buf`; the returned
/// slice excludes the terminator.
///
/// Here is the logic for macOS, but other platforms also have
/// their own canonicalization logic:
///
/// https://github.com/coreutils/gnulib/blob/5b92dd0a45c8d27f13a21076b57095ea5e220870/lib/localename.c#L1171
///
/// `_libintl_locale_name_canonicalize` is only exported by the libintl
/// Ghostty builds from source (macOS). Elsewhere (glibc) the locale is
/// copied unchanged after the zh fixups.
pub fn canonicalize_locale<'a>(buf: &'a mut [u8], locale: &str) -> Result<&'a str, NoSpaceLeft> {
    if !BUILD_I18N {
        return copy_nul(buf, locale.as_bytes());
    }

    // Fix zh locales for macOS
    if let Some(fixed) = fix_zh_locale(locale) {
        return copy_nul(buf, fixed.as_bytes());
    }

    // Buffer must be 16 or at least as long as the locale and null term
    if buf.len() < 16.max(locale.len() + 1) {
        return Err(NoSpaceLeft);
    }

    // Copy our locale into the buffer since it modifies in place.
    // This must be null-terminated.
    buf[..locale.len()].copy_from_slice(locale.as_bytes());
    buf[locale.len()] = 0;

    #[cfg(target_os = "macos")]
    // SAFETY: buf is NUL-terminated and at least 16 bytes.
    unsafe {
        ffi::_libintl_locale_name_canonicalize(buf.as_mut_ptr().cast())
    };

    // Convert the null-terminated result buffer into a slice. We
    // need to search for the null terminator and slice it back.
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..len]).map_err(|_| NoSpaceLeft)
}

/// Handles some zh locales canonicalization because internal libintl
/// canonicalization function doesn't handle correctly in these cases.
fn fix_zh_locale(locale: &str) -> Option<&'static str> {
    let mut it = locale.split('-');
    let name = it.next()?;
    if name != "zh" {
        return None;
    }

    let script = it.next()?;
    let region = it.next()?;

    if script == "Hans" {
        if region == "SG" {
            return Some("zh_SG");
        }
        return Some("zh_CN");
    }

    if script == "Hant" {
        if region == "MO" {
            return Some("zh_MO");
        }
        if region == "HK" {
            return Some("zh_HK");
        }
        return Some("zh_TW");
    }

    None
}

/// This can be called at any point a compile-time-known locale is
/// available. In a `const` context an unsupported locale fails
/// compilation ("unsupported locale").
pub const fn static_locale(v: &'static str) -> &'static str {
    let mut i = 0;
    while i < LOCALES.len() {
        let locale = LOCALES[i];
        let a = locale.as_bytes();
        let b = v.as_bytes();
        if a.len() == b.len() {
            let mut j = 0;
            while j < a.len() && a[j] == b[j] {
                j += 1;
            }
            if j == a.len() {
                return locale;
            }
        }
        i += 1;
    }
    panic!("unsupported locale");
}

// Manually include function definitions for the gettext functions
// as libintl.h isn't always easily available (e.g. in musl). On glibc
// these live in libc itself.
#[cfg(not(windows))]
mod ffi {
    use super::c_char;

    extern "C" {
        pub fn bindtextdomain(domainname: *const c_char, dirname: *const c_char) -> *const c_char;
        pub fn textdomain(domainname: *const c_char) -> *const c_char;
        pub fn dgettext(domainname: *const c_char, msgid: *const c_char) -> *const c_char;

        // This is only available if we're building libintl from source
        // since its otherwise not exported. We only need it on macOS
        // currently but probably will on Windows as well.
        #[cfg(target_os = "macos")]
        pub fn _libintl_locale_name_canonicalize(name: *mut c_char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_vendor = "apple")]
    #[test]
    fn canonicalize_locale_darwin() {
        let mut buf = [0u8; 256];
        assert_eq!("en_US", canonicalize_locale(&mut buf, "en_US").unwrap());
        assert_eq!("zh_CN", canonicalize_locale(&mut buf, "zh-Hans").unwrap());
        assert_eq!("zh_TW", canonicalize_locale(&mut buf, "zh-Hant").unwrap());

        assert_eq!(
            "zh_CN",
            canonicalize_locale(&mut buf, "zh-Hans-CN").unwrap()
        );
        assert_eq!(
            "zh_SG",
            canonicalize_locale(&mut buf, "zh-Hans-SG").unwrap()
        );
        assert_eq!(
            "zh_TW",
            canonicalize_locale(&mut buf, "zh-Hant-TW").unwrap()
        );
        assert_eq!(
            "zh_HK",
            canonicalize_locale(&mut buf, "zh-Hant-HK").unwrap()
        );
        assert_eq!(
            "zh_MO",
            canonicalize_locale(&mut buf, "zh-Hant-MO").unwrap()
        );

        // This is just an edge case I want to make sure we're aware of:
        // canonicalizeLocale does not handle encodings and will turn them
        // into underscores. We should parse them out before calling this
        // function.
        assert_eq!(
            "en_US.UTF_8",
            canonicalize_locale(&mut buf, "en_US.UTF-8").unwrap()
        );
    }

    #[test]
    fn canonicalize_locale_zh_fixups_and_bounds() {
        let mut buf = [0u8; 256];
        assert_eq!(
            "zh_CN",
            canonicalize_locale(&mut buf, "zh-Hans-CN").unwrap()
        );
        assert_eq!(
            "zh_SG",
            canonicalize_locale(&mut buf, "zh-Hans-SG").unwrap()
        );
        assert_eq!(
            "zh_HK",
            canonicalize_locale(&mut buf, "zh-Hant-HK").unwrap()
        );
        assert_eq!(
            "zh_MO",
            canonicalize_locale(&mut buf, "zh-Hant-MO").unwrap()
        );
        assert_eq!(
            "zh_TW",
            canonicalize_locale(&mut buf, "zh-Hant-TW").unwrap()
        );
        assert_eq!(None, fix_zh_locale("zh-Hans"));
        assert_eq!(None, fix_zh_locale("en-Hans-US"));
        let mut small = [0u8; 8];
        assert_eq!(Err(NoSpaceLeft), canonicalize_locale(&mut small, "en_US"));
        assert_eq!(
            Err(NoSpaceLeft),
            canonicalize_locale(&mut small[..5], "zh-Hans-CN")
        );
    }

    #[test]
    fn underscore_returns_msgid_at_comptime() {
        const MSGID: &CStr = gettext_const(c"Ghostty");
        assert_eq!(c"Ghostty", MSGID);
        // Without a bound catalog, runtime translation is the identity too.
        assert_eq!(c"Ghostty", gettext(c"Ghostty"));
        assert_eq!(c"Ghostty", N_(c"Ghostty"));
    }

    #[test]
    fn static_locale_and_map() {
        const DE: &str = static_locale("de");
        assert_eq!("de", DE);
        assert!(LOCALES_MAP.contains("sr@latin"));
        assert!(!LOCALES_MAP.contains("xx"));
    }

    #[test]
    fn init_requires_share_dir() {
        if BUILD_I18N {
            assert_eq!(Err(InitError::InvalidResourcesDir), init("ghostty"));
            init("/usr/share/ghostty").unwrap();
        }
        init_global_domain().unwrap();
    }
}
