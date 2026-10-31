//! Port of `src/renderer/backend.zig`.
//!
//! STATUS: ported

/// Possible implementations, used for build options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Opengl,
    Metal,
}

impl Backend {
    /// The default backend for the target being compiled for.
    ///
    /// The Zig version takes the `std.Target` and the wasm target (which
    /// it ignores) as parameters since it runs in the build system; here
    /// the compilation target is known through `cfg`.
    pub const fn default() -> Backend {
        // Zig: `target.os.tag.isDarwin()`.
        if cfg!(target_vendor = "apple") {
            return Backend::Metal;
        }
        Backend::Opengl
    }

    /// The Zig tag name (used as the build option value).
    pub const fn as_str(self) -> &'static str {
        match self {
            Backend::Opengl => "opengl",
            Backend::Metal => "metal",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Backend {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "opengl" => Ok(Backend::Opengl),
            "metal" => Ok(Backend::Metal),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backend() {
        #[cfg(target_os = "linux")]
        assert_eq!(Backend::Opengl, Backend::default());
        #[cfg(target_os = "macos")]
        assert_eq!(Backend::Metal, Backend::default());
        assert_eq!(Ok(Backend::Metal), "metal".parse());
        assert_eq!("opengl", Backend::Opengl.as_str());
    }
}
