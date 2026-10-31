//! Port of `src/extra/pkg-config.zig`.
//!
//! STATUS: ported
//!
//! Generates a `pkg-config` .pc file for `libghostty-vt`.
//!
//! This is required as of Zig 0.17 since the configurer process that runs
//! `build.zig` build scripts can no longer access the install prefix as a
//! simple string and use it to template files.
//!
//! Zig receives the values through the build-generated `options` module
//! (see `build/GhosttyPkgConfig.zig`); in Rust they are passed in as
//! [`Options`], and [`Options::LIBGHOSTTY_VT`] holds the values the Zig
//! build uses for `libghostty-vt` (`build/GhosttyLibVt.zig`) on this target
//! with no system integration.

use std::fmt;
use std::io::{self, Write};

use crate::build_config::{self, SemanticVersion};

/// The `options` build module of the generator (all lists already joined
/// with spaces, optional values already defaulted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub name: &'static str,
    pub name_static: &'static str,
    pub description: &'static str,
    pub description_static: &'static str,
    pub version: SemanticVersion,
    pub libs: &'static str,
    pub libs_static: &'static str,
    pub libs_private: &'static str,
    pub reqs_private: &'static str,
}

impl Options {
    /// The options for `libghostty-vt` (`GhosttyLibVt.zig`
    /// `pkgConfigFiles`).
    pub const LIBGHOSTTY_VT: Options = Options {
        name: "ghostty-vt",
        name_static: "ghostty-vt-static",
        description: "Ghostty VT library",
        description_static: "Ghostty VT library (static)",
        version: build_config::VERSION,
        libs: "-L${libdir} -lghostty-vt",
        libs_static: if cfg!(windows) {
            "${libdir}/ghostty-vt-static.lib"
        } else {
            "${libdir}/libghostty-vt.a"
        },
        // `libsPrivate`: "-lc++" only when the VT C library links libc++.
        libs_private: "",
        // `requiresPrivate`: no system simdutf/highway integration.
        reqs_private: "",
    };
}

#[derive(Debug)]
pub enum Error {
    /// No install prefix argument was given.
    NoPrefix,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoPrefix => f.write_str("NoPrefix"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Write the .pc file contents for the given install prefix.
pub fn write<W: Write + ?Sized>(
    writer: &mut W,
    options: &Options,
    prefix: &str,
    is_static: bool,
) -> io::Result<()> {
    write!(
        writer,
        "prefix={prefix}\n\
         includedir=${{prefix}}/include\n\
         libdir=${{prefix}}/lib\n\
         \n\
         Name: {name}\n\
         URL: https://github.com/ghostty-org/ghostty\n\
         Description: {description}\n\
         Version: {version}\n\
         Cflags: -I${{includedir}}\n\
         Libs: {libs}\n\
         Libs.private: {libs_private}\n\
         Requires.private: {reqs_private}",
        prefix = prefix,
        name = if is_static { options.name_static } else { options.name },
        description = if is_static {
            options.description_static
        } else {
            options.description
        },
        version = options.version,
        libs = if is_static { options.libs_static } else { options.libs },
        libs_private = options.libs_private,
        reqs_private = options.reqs_private,
    )
}

/// The generator's entry point: `pkg-config-generator <prefix>`, writing
/// the static variant when `IS_STATIC` is set in the environment.
pub fn main(options: &Options) -> Result<(), Error> {
    let mut args = std::env::args();
    let _ = args.next();
    let prefix = args.next().ok_or(Error::NoPrefix)?;

    let is_static = std::env::var_os("IS_STATIC").is_some();

    let stdout = io::stdout();
    let mut stdout_writer = stdout.lock();
    write(&mut stdout_writer, options, &prefix, is_static)?;
    stdout_writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTS: Options = Options {
        name: "ghostty-vt",
        name_static: "ghostty-vt-static",
        description: "Ghostty VT library",
        description_static: "Ghostty VT library (static)",
        version: SemanticVersion {
            major: 1,
            minor: 2,
            patch: 3,
            pre: Some("dev"),
            build: None,
        },
        libs: "-L${libdir} -lghostty-vt",
        libs_static: "${libdir}/libghostty-vt.a",
        libs_private: "-lc++",
        reqs_private: "simdutf",
    };

    #[test]
    fn pkg_config_shared() {
        let mut out = Vec::new();
        write(&mut out, &OPTS, "/usr", false).unwrap();
        assert_eq!(
            "prefix=/usr\n\
             includedir=${prefix}/include\n\
             libdir=${prefix}/lib\n\
             \n\
             Name: ghostty-vt\n\
             URL: https://github.com/ghostty-org/ghostty\n\
             Description: Ghostty VT library\n\
             Version: 1.2.3-dev\n\
             Cflags: -I${includedir}\n\
             Libs: -L${libdir} -lghostty-vt\n\
             Libs.private: -lc++\n\
             Requires.private: simdutf",
            String::from_utf8(out).unwrap()
        );
    }

    #[test]
    fn pkg_config_static() {
        let mut out = Vec::new();
        write(&mut out, &OPTS, "/opt", true).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Name: ghostty-vt-static\n"));
        assert!(s.contains("Description: Ghostty VT library (static)\n"));
        assert!(s.contains("Libs: ${libdir}/libghostty-vt.a\n"));
        assert_eq!(Options::LIBGHOSTTY_VT.name, "ghostty-vt");
    }
}
