//! Port of `src/os/locale.zig`.
//!
//! STATUS: ported

use std::ffi::{c_char, CStr};

/// `getenv` via libc so we observe values set with `setenv` below.
fn getenv(name: &CStr) -> Option<&'static CStr> {
    // SAFETY: valid NUL-terminated name; the returned pointer is owned by
    // the environment.
    let p = unsafe { libc::getenv(name.as_ptr()) };
    if p.is_null() {
        None
    } else {
        // SAFETY: getenv returns a NUL-terminated string.
        Some(unsafe { CStr::from_ptr(p) })
    }
}

fn setlocale_all(locale: &CStr) -> Option<&'static CStr> {
    // SAFETY: valid NUL-terminated locale string.
    let p = unsafe { libc::setlocale(libc::LC_ALL, locale.as_ptr()) };
    if p.is_null() {
        None
    } else {
        // SAFETY: setlocale returns a NUL-terminated string.
        Some(unsafe { CStr::from_ptr(p) })
    }
}

fn setenv(name: &CStr, value: &CStr) -> i32 {
    #[cfg(unix)]
    // SAFETY: valid NUL-terminated strings. Callers must not race with
    // other threads reading the environment (see `ensure_locale`).
    return unsafe { libc::setenv(name.as_ptr(), value.as_ptr(), 1) };
    #[cfg(not(unix))]
    {
        // SAFETY: see above.
        unsafe { std::env::set_var(name.to_str().unwrap_or(""), value.to_str().unwrap_or("")) };
        0
    }
}

fn unsetenv(name: &CStr) -> i32 {
    #[cfg(unix)]
    // SAFETY: valid NUL-terminated string.
    return unsafe { libc::unsetenv(name.as_ptr()) };
    #[cfg(not(unix))]
    {
        // SAFETY: see `setenv`.
        unsafe { std::env::remove_var(name.to_str().unwrap_or("")) };
        0
    }
}

/// Ensure that the locale is set.
///
/// This does a lot of in-process mutation of the environment and can't be
/// run in a test as a result. It must be called early during startup,
/// before other threads exist.
pub fn ensure_locale() {
    // This does a lot of in-process mutation of the environment and can't
    // be run in a test as a result.
    debug_assert!(!cfg!(test));

    // On macOS, pre-populate the LANG env var with system preferences.
    // When launching the .app, LANG is not set so we must query it from
    // the OS. When launching from the CLI, LANG is usually set by the
    // parent process.
    #[cfg(target_os = "macos")]
    {
        // Set the lang if it is not set or if its empty.
        let lang = getenv(c"LANG");
        if lang.is_none_or(|l| l.is_empty()) {
            set_lang_from_cocoa();
        }
    }

    // Set the locale to whatever is set in env vars.
    if let Some(v) = setlocale_all(c"") {
        log::info!(target: "os_locale", "setlocale from env result={}", v.to_string_lossy());
        return;
    }

    if cfg!(windows) {
        // Exit early for Windows.
        //
        // NOTE: There currently is no official Windows version of Ghostty.
        // setlocale on Windows sets an "implementation-defined native
        // environment" with an empty string, so we are currently just doing
        // best-effort for now.
        log::info!(target: "os_locale", "TODO: setlocale failed on Windows, implement better fallbacks");
        return;
    }

    // setlocale failed. This is probably because the LANG env var is
    // invalid. Try to set it without the LANG var set to use the system
    // default.
    if let Some(lang) = getenv(c"LANG") {
        if !lang.is_empty() {
            // We don't need to do both of these things but we do them
            // both to be sure that lang is either empty or unset completely.
            setenv(c"LANG", c"");
            unsetenv(c"LANG");

            if let Some(v) = setlocale_all(c"") {
                log::info!(target: "os_locale", "setlocale after unset lang result={}", v.to_string_lossy());

                // If we try to setlocale to an unsupported locale it'll
                // return "C" as the POSIX/C fallback, if that's the case we
                // want to not use it and move to our fallback of en_US.UTF-8
                if v.to_bytes() != b"C" {
                    return;
                }
            }
        }
    }

    // Failure again... fallback to en_US.UTF-8
    log::warn!(
        target: "os_locale",
        "setlocale failed with LANG and system default. Falling back to en_US.UTF-8"
    );
    if let Some(v) = setlocale_all(c"en_US.UTF-8") {
        setenv(c"LANG", c"en_US.UTF-8");
        log::info!(target: "os_locale", "setlocale default result={}", v.to_string_lossy());
    } else {
        log::warn!(target: "os_locale", "setlocale failed even with the fallback, uncertain results");
    }
}

/// This sets the LANG environment variable based on the macOS system
/// preferences selected locale settings.
#[cfg(target_os = "macos")]
fn set_lang_from_cocoa() {
    use super::macos::objc;

    // SAFETY: autorelease pool push/pop pair.
    let pool = unsafe { objc::objc_autoreleasePoolPush() };
    struct Pool(*mut std::ffi::c_void);
    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: popping the pool we pushed.
            unsafe { objc::objc_autoreleasePoolPop(self.0) };
        }
    }
    let _pool = Pool(pool);

    // The classes we're going to need.
    let Some(ns_locale) = objc::get_class(c"NSLocale") else {
        log::warn!(target: "os_locale", "NSLocale class not found. Locale may be incorrect.");
        return;
    };

    // Get our current locale and extract the language code ("en") and
    // country code ("US")
    // SAFETY: standard Foundation selectors and signatures.
    let (z_lang, z_country) = unsafe {
        let locale = objc::msg_send_id(ns_locale, objc::sel(c"currentLocale"));
        let lang = objc::msg_send_id(locale, objc::sel(c"languageCode"));
        let country = objc::msg_send_id(locale, objc::sel(c"countryCode"));
        if lang.is_null() || country.is_null() {
            log::warn!(target: "os_locale", "languageCode or countryCode not found. Locale may be incorrect.");
            return;
        }
        match (
            objc::ns_string_to_string(lang),
            objc::ns_string_to_string(country),
        ) {
            (Some(l), Some(c)) => (l, c),
            _ => return,
        }
    };

    // Format our locale as "<lang>_<country>.UTF-8" and set it as LANG.
    {
        let env_value = format!("{z_lang}_{z_country}.UTF-8");
        if env_value.len() >= 128 {
            log::warn!(target: "os_locale", "error setting locale from system. err=NoSpaceLeft");
            return;
        }
        log::info!(target: "os_locale", "detected system locale={env_value}");

        // Set it onto our environment
        let c = std::ffi::CString::new(env_value).unwrap();
        if setenv(c"LANG", &c) < 0 {
            log::warn!(target: "os_locale", "error setting locale env var");
            return;
        }
    }

    // Get our preferred languages and set that to the LANGUAGE
    // env var in case our language differs from our locale.
    let mut buf = [0u8; 1024];
    match preferred_language_from_cocoa(&mut buf, ns_locale) {
        Err(err) => {
            log::warn!(target: "os_locale", "error getting preferred languages. err={err}");
        }
        Ok(None) => {}
        Ok(Some(pref)) => {
            log::debug!(target: "os_locale", "setting LANGUAGE from preferred languages value={pref}");
            let c = std::ffi::CString::new(pref).unwrap();
            setenv(c"LANGUAGE", &c);
        }
    }
}

/// Sets the LANGUAGE environment variable based on the preferred languages
/// as reported by NSLocale.
///
/// macOS has a concept of preferred languages separate from the system
/// locale. The set of preferred languages is a list in priority order
/// of what translations the user prefers. A user can have, for example,
/// "fr_FR" as their locale but "en" as their preferred language. This would
/// mean that they want to use French units, date formats, etc. but they
/// prefer English translations.
///
/// gettext uses the LANGUAGE environment variable to override only
/// translations and a priority order can be specified by separating
/// the languages with colons. For example, "en:fr" would mean that
/// English translations are preferred but if they are not available
/// then French translations should be used.
///
/// To further complicate things, Apple reports the languages in BCP-47
/// format which is not compatible with gettext's POSIX locale format so
/// we have to canonicalize them.
#[cfg(target_os = "macos")]
fn preferred_language_from_cocoa(
    buf: &mut [u8],
    ns_locale: super::macos::objc::Id,
) -> Result<Option<String>, super::i18n::NoSpaceLeft> {
    use super::i18n::{self, NoSpaceLeft};
    use super::macos::objc;

    let mut end = 0usize;

    // We need to get our app's preferred languages. These may not
    // match the system locale (NSLocale.currentLocale).
    // SAFETY: standard Foundation selectors and signatures.
    unsafe {
        let preferred = objc::msg_send_id(ns_locale, objc::sel(c"preferredLanguages"));
        let count = objc::msg_send_usize(preferred, objc::sel(c"count"));
        for i in 0..count {
            let str = objc::msg_send_id_usize(preferred, objc::sel(c"objectAtIndex:"), i);
            let Some(c_str) = objc::ns_string_to_string(str) else {
                // I don't think this can happen but if it does then I want
                // to know about it if a user has translation issues.
                log::warn!(target: "os_locale", "failed to convert a preferred language to UTF-8");
                continue;
            };

            // Append our separator if we have any previous languages
            if end > 0 {
                if end >= buf.len() {
                    return Err(NoSpaceLeft);
                }
                buf[end] = b':';
                end += 1;
            }

            // Apple languages are in BCP-47 format, and we need to
            // canonicalize them to the POSIX format.
            let canon_len = i18n::canonicalize_locale(&mut buf[end..], &c_str)?.len();
            end += canon_len;

            // The canonicalized locale never contains the encoding and
            // all of our translations require UTF-8 so we add that.
            let suffix = b".UTF-8";
            if end + suffix.len() > buf.len() {
                return Err(NoSpaceLeft);
            }
            buf[end..end + suffix.len()].copy_from_slice(suffix);
            end += suffix.len();
        }
    }

    // If we had no preferred languages then we return nothing.
    if end == 0 {
        return Ok(None);
    }

    // Null terminate it (checks the space like Zig does).
    if end >= buf.len() {
        return Err(NoSpaceLeft);
    }
    buf[end] = 0;

    Ok(Some(String::from_utf8_lossy(&buf[..end]).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getenv_and_setlocale_query() {
        // Querying (null locale is not exposed); setting the current
        // locale to itself must succeed without mutating the environment.
        // SAFETY: a null locale argument queries the current locale.
        let current = unsafe { libc::setlocale(libc::LC_ALL, std::ptr::null()) };
        assert!(!current.is_null());
        // SAFETY: setlocale returns a NUL-terminated string.
        let current = unsafe { CStr::from_ptr(current) }.to_owned();
        assert!(setlocale_all(&current).is_some());
        assert!(getenv(c"GHOSTTY_SURELY_UNSET_VARIABLE_123").is_none());
        let _ = c_char::default();
    }
}
