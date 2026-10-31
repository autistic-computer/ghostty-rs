//! Port of `src/cli/show_face.zig`.
//!
//! STATUS: ported

use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct, ArgIterator, Error, Fields, ParseHooks};
use super::diagnostics::{DiagnosticList, LocationKey};
use super::ghostty::{global_args, Action, RunResult};
use crate::config::Config;
use crate::font;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The codepoint to search for.
        pub cp: Option<u32> = None,

        /// Search for all of the codepoints in the string.
        pub string: Option<String> = None,

        /// Font style to search for.
        pub style: font::Style = font::Style::Regular,

        /// If specified, force text or emoji presentation.
        pub presentation: Option<font::Presentation> = None,
    }
    private {
        // Enable arg parsing diagnostics so that we don't get an error if
        // there is a "normal" config setting on the cli.
        pub _diagnostics: DiagnosticList = DiagnosticList::default(),
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    fn diagnostics_mut(&mut self) -> Option<&mut DiagnosticList> {
        Some(&mut self._diagnostics)
    }

    /// Enables "-h" and "--help" to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"show-face"`).
pub const HELP: &str = include_str!("show_face_help.txt");

/// The `show-face` command shows what font face Ghostty will use to render a
/// specific codepoint. See [`HELP`].
pub fn run() -> RunResult {
    let mut iter = args::args_iterator(global_args());

    let mut stdout = BufWriter::with_capacity(1024, io::stdout().lock());
    // Note: like the Zig version, errors go to stdout as well.
    let mut stderr: Vec<u8> = Vec::new();

    let result = run_args(&mut iter, &mut stdout, &mut stderr);
    let _ = stdout.write_all(&stderr);
    let _ = stdout.flush();
    result
}

fn run_args<I: ArgIterator + ?Sized, O: Write + ?Sized, E: Write + ?Sized>(
    args_iter: &mut I,
    stdout: &mut O,
    stderr: &mut E,
) -> RunResult {
    // Its possible to build Ghostty without font discovery!
    if !super::list_fonts::DISCOVERY_AVAILABLE {
        write!(
            stderr,
            "Ghostty was built without a font discovery mechanism. This is a compile-time
option. Please review how Ghostty was built from source, contact the
maintainer to enable a font discovery mechanism, and try again."
        )?;
        return Ok(1);
    }

    let mut opts = Options::default();

    match args::parse(&mut opts, args_iter) {
        Ok(()) => {}
        Err(err) if err == Action::HELP_ERROR => return Err(Box::new(err)),
        Err(err) => {
            writeln!(stderr, "Error parsing args: {}", err)?;
            return Ok(1);
        }
    }

    // Print out any diagnostics, unless it's likely that the diagnostic was
    // generated trying to parse a "normal" configuration setting. Exit with an
    // error code if any diagnostics were printed.
    if !opts._diagnostics.empty() {
        let mut exit = false;
        for diagnostic in opts._diagnostics.items() {
            if diagnostic.location.key() != LocationKey::Cli {
                continue;
            }
            for field in Options::FIELDS {
                if field.name == diagnostic.key.as_str() {
                    writeln!(stderr, "config error: {}", diagnostic)?;
                    exit = true;
                }
            }
        }
        if exit {
            return Ok(1);
        }
    }

    if opts.cp.is_none() && opts.string.is_none() {
        writeln!(stderr, "You must specify a codepoint with --cp or a string with --string")?;
        return Ok(1);
    }

    let config = match Config::load() {
        Ok(config) => config,
        Err(err) => {
            write!(stderr, "Unable to load config: {}", err)?;
            return Ok(1);
        }
    };

    // Print out any diagnostics generated from parsing the config, unless
    // the diagnostic might have been generated because it's actually an
    // action-specific argument.
    if !config._diagnostics.empty() {
        'outer: for diagnostic in config._diagnostics.items() {
            for field in Options::FIELDS {
                if field.name == diagnostic.key.as_str()
                    && matches!(diagnostic.location.key(), LocationKey::None | LocationKey::Cli)
                {
                    continue 'outer;
                }
            }
            writeln!(stderr, "config error: {}", diagnostic)?;
        }
    }

    let mut font_grid_set = match font::SharedGridSet::init() {
        Ok(v) => v,
        Err(err) => {
            write!(stderr, "Unable to initialize font grid set: {}", err)?;
            return Ok(1);
        }
    };

    let font_size = font::face::DesiredSize {
        points: config.font_size,
        xdpi: 96,
        ydpi: 96,
    };

    let font_config = font::shared_grid_set::DerivedConfig::init(&config);

    let (font_grid_key, font_grid) = match font_grid_set.r#ref(&font_config, font_size) {
        Ok(v) => v,
        Err(err) => {
            write!(stderr, "Unable to get font grid: {}", err)?;
            return Ok(1);
        }
    };

    // Zig: `defer font_grid_set.deref(font_grid_key)`.
    let result = (|| -> RunResult {
        if let Some(cp) = opts.cp {
            if let Some(rc) = lookup(stdout, stderr, &font_grid, opts.style, opts.presentation, cp)? {
                return Ok(rc);
            }
        }
        if let Some(string) = &opts.string {
            for c in string.chars() {
                if let Some(rc) =
                    lookup(stdout, stderr, &font_grid, opts.style, opts.presentation, c as u32)?
                {
                    return Ok(rc);
                }
            }
        }

        Ok(0)
    })();
    drop(font_grid);
    font_grid_set.deref(&font_grid_key);
    result
}

/// Zig `{X:0>2}` and `{u}` of a codepoint.
fn cp_strings(cp: u32) -> (String, String) {
    (
        format!("{:02X}", cp),
        char::from_u32(cp).map(String::from).unwrap_or_else(|| "\u{FFFD}".to_owned()),
    )
}

fn lookup<O: Write + ?Sized, E: Write + ?Sized>(
    stdout: &mut O,
    stderr: &mut E,
    font_grid: &font::SharedGrid,
    style: font::Style,
    presentation: Option<font::Presentation>,
    cp: u32,
) -> io::Result<Option<u8>> {
    let (hex, ch) = cp_strings(cp);
    let mut state = font_grid.lock.write().unwrap_or_else(|e| e.into_inner());
    let Some(idx) = state.resolver.get_index(cp, style, presentation) else {
        writeln!(stdout, "U+{} « {} » not found.", hex, ch)?;
        return Ok(None);
    };

    let face = match state.resolver.collection.get_face(idx) {
        Ok(face) => face,
        Err(font::collection::GetFaceError::SpecialHasNoFace) => {
            writeln!(stdout, "U+{} « {} » is handled by Ghostty's internal sprites.", hex, ch)?;
            return Ok(None);
        }
        Err(err) => {
            write!(stderr, "Unable to get face: {}", err)?;
            return Ok(Some(1));
        }
    };

    let mut buf = [0u8; 1024];
    let name = match face.name(&mut buf) {
        Ok(name) => String::from_utf8_lossy(name).into_owned(),
        Err(err) => {
            write!(stderr, "Unable to get name of face: {}", err)?;
            return Ok(Some(1));
        }
    };

    writeln!(stdout, "U+{} « {} » found in face “{}”.", hex, ch, name)?;

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ArgIteratorGeneral;

    #[test]
    fn parse_options() {
        let mut opts = Options::default();
        let mut iter = ArgIteratorGeneral::init("--cp=0x41 --style=bold_italic --presentation=emoji --font-size=12");
        args::parse(&mut opts, &mut iter).unwrap();
        assert_eq!(Some(0x41), opts.cp);
        assert_eq!(font::Style::BoldItalic, opts.style);
        assert_eq!(Some(font::Presentation::Emoji), opts.presentation);
        // The unknown (config) field becomes a diagnostic, not an error.
        assert_eq!(1, opts._diagnostics.items().len());
    }

    #[test]
    fn requires_cp_or_string() {
        let mut iter = ArgIteratorGeneral::init("");
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        assert_eq!(1, run_args(&mut iter, &mut out, &mut err).unwrap());
        assert!(String::from_utf8_lossy(&err).contains("You must specify"));
    }

    #[test]
    fn sprite_codepoints() {
        let mut set = font::SharedGridSet::init().unwrap();
        let config = font::shared_grid_set::DerivedConfig::init(&Config::default());
        let (key, grid) = set.r#ref(&config, font::face::DesiredSize::new(12.0)).unwrap();

        // Box drawing is handled by the sprite face.
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let rc = lookup(&mut out, &mut err, &grid, font::Style::Regular, None, 0x2500).unwrap();
        assert_eq!(None, rc);
        assert!(String::from_utf8_lossy(&out).contains("internal sprites"));

        drop(grid);
        set.deref(&key);
    }
}
