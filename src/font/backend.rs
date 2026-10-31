//! Port of `src/font/backend.zig`.
//!
//! STATUS: ported

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// FreeType for font rendering with no font discovery enabled.
    Freetype,

    /// FreeType for font rendering with a built-in Windows font directory
    /// scanner (C:\Windows\Fonts + %LOCALAPPDATA%\Microsoft\Windows\Fonts).
    /// Used when DirectWrite is not available; matches by family_name and
    /// SFNT name table without any external index.
    FreetypeWindows,

    /// Fontconfig for font discovery and FreeType for font rendering.
    FontconfigFreetype,

    /// CoreText for font discovery, rendering, and shaping (macOS).
    Coretext,

    /// CoreText for font discovery, FreeType for rendering, and
    /// HarfBuzz for shaping (macOS).
    CoretextFreetype,

    /// CoreText for font discovery and rendering, HarfBuzz for shaping
    CoretextHarfbuzz,

    /// CoreText for font discovery and rendering, no shaping.
    CoretextNoshape,

    /// Use the browser font system and the Canvas API (wasm). This limits
    /// the available fonts to browser fonts (anything Canvas natively
    /// supports).
    WebCanvas,
}

/// The wasm target kind, used by [`Backend::default`].
pub use crate::os::wasm::target::Target as WasmTarget;

/// The parts of a build target (Zig `std.Target`) that [`Backend::default`]
/// looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub is_wasm32: bool,
    pub is_windows: bool,
    pub is_darwin: bool,
}

impl Target {
    /// The target this crate is being compiled for.
    pub const fn current() -> Target {
        Target {
            is_wasm32: cfg!(target_arch = "wasm32"),
            is_windows: cfg!(target_os = "windows"),
            is_darwin: cfg!(any(
                target_os = "macos",
                target_os = "ios",
                target_os = "tvos",
                target_os = "watchos",
                target_os = "visionos"
            )),
        }
    }
}

impl Backend {
    /// The original (Zig) spelling of the backend name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Backend::Freetype => "freetype",
            Backend::FreetypeWindows => "freetype_windows",
            Backend::FontconfigFreetype => "fontconfig_freetype",
            Backend::Coretext => "coretext",
            Backend::CoretextFreetype => "coretext_freetype",
            Backend::CoretextHarfbuzz => "coretext_harfbuzz",
            Backend::CoretextNoshape => "coretext_noshape",
            Backend::WebCanvas => "web_canvas",
        }
    }

    /// Returns the default backend for a build environment. This is
    /// meant to be called at comptime by the build.zig script. To get the
    /// backend look at build_options.
    pub const fn default(target: Target, wasm_target: WasmTarget) -> Backend {
        if target.is_wasm32 {
            return match wasm_target {
                WasmTarget::Browser => Backend::WebCanvas,
            };
        }

        if target.is_windows {
            // Avoid fontconfig on Windows because its libxml2 dependency
            // may not unpack due to symlinks. Use the FreeType-based
            // Windows font-directory scanner for discovery. A future
            // DirectWrite backend can replace this if needed.
            return Backend::FreetypeWindows;
        }

        // macOS also supports "coretext_freetype" but there is no scenario
        // that is the default. It is only used by people who want to
        // self-compile Ghostty and prefer the freetype aesthetic.
        if target.is_darwin {
            Backend::Coretext
        } else {
            Backend::FontconfigFreetype
        }
    }

    // All the functions below can be called at comptime or runtime to
    // determine if we have a certain dependency.

    pub const fn has_freetype(self) -> bool {
        match self {
            Backend::Freetype
            | Backend::FreetypeWindows
            | Backend::FontconfigFreetype
            | Backend::CoretextFreetype => true,

            Backend::Coretext
            | Backend::CoretextHarfbuzz
            | Backend::CoretextNoshape
            | Backend::WebCanvas => false,
        }
    }

    pub const fn has_coretext(self) -> bool {
        match self {
            Backend::Coretext
            | Backend::CoretextFreetype
            | Backend::CoretextHarfbuzz
            | Backend::CoretextNoshape => true,

            Backend::Freetype
            | Backend::FreetypeWindows
            | Backend::FontconfigFreetype
            | Backend::WebCanvas => false,
        }
    }

    pub const fn has_fontconfig(self) -> bool {
        match self {
            Backend::FontconfigFreetype => true,

            Backend::Freetype
            | Backend::FreetypeWindows
            | Backend::Coretext
            | Backend::CoretextFreetype
            | Backend::CoretextHarfbuzz
            | Backend::CoretextNoshape
            | Backend::WebCanvas => false,
        }
    }

    pub const fn has_harfbuzz(self) -> bool {
        match self {
            Backend::Freetype
            | Backend::FreetypeWindows
            | Backend::FontconfigFreetype
            | Backend::CoretextFreetype
            | Backend::CoretextHarfbuzz => true,

            Backend::Coretext | Backend::CoretextNoshape | Backend::WebCanvas => false,
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backend() {
        let t = |w, win, d| Target {
            is_wasm32: w,
            is_windows: win,
            is_darwin: d,
        };
        assert_eq!(
            Backend::WebCanvas,
            Backend::default(t(true, false, false), WasmTarget::Browser)
        );
        assert_eq!(
            Backend::FreetypeWindows,
            Backend::default(t(false, true, false), WasmTarget::Browser)
        );
        assert_eq!(
            Backend::Coretext,
            Backend::default(t(false, false, true), WasmTarget::Browser)
        );
        assert_eq!(
            Backend::FontconfigFreetype,
            Backend::default(t(false, false, false), WasmTarget::Browser)
        );
        assert!(Backend::FontconfigFreetype.has_freetype());
        assert!(Backend::FontconfigFreetype.has_fontconfig());
        assert!(Backend::FontconfigFreetype.has_harfbuzz());
        assert!(!Backend::FontconfigFreetype.has_coretext());
    }
}
