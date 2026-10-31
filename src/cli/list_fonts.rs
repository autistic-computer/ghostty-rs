//! Port of `src/cli/list_fonts.zig`.
//!
//! STATUS: ported

use std::collections::HashMap;
use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct, ArgIterator, Error, ParseHooks, StringZ};
use super::ghostty::{global_args, Action, RunResult};
use crate::font;

const LOG: &str = "list_fonts";

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The font family to search for. If this is set, then only fonts
        /// matching this family will be listed.
        pub family: Option<StringZ> = None,

        /// The style name to search for.
        pub style: Option<StringZ> = None,

        /// Font styles to search for. If this is set, then only fonts that
        /// match the given styles will be listed.
        pub bold: bool = false,
        pub italic: bool = false,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables "-h" and "--help" to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"list-fonts"`).
pub const HELP: &str = include_str!("list_fonts_help.txt");

/// The `list-fonts` command is used to list all the available fonts for
/// Ghostty. See [`HELP`].
pub fn run() -> RunResult {
    let mut iter = args::args_iterator(global_args());
    run_args(&mut iter)
}

fn run_args<I: ArgIterator + ?Sized>(args_iter: &mut I) -> RunResult {
    let mut config = Options::default();
    args::parse(&mut config, args_iter)?;

    // Its possible to build Ghostty without font discovery!
    if !DISCOVERY_AVAILABLE {
        let mut stderr = io::stderr().lock();
        write!(
            stderr,
            "Ghostty was built without a font discovery mechanism. This is a compile-time
option. Please review how Ghostty was built from source, contact the
maintainer to enable a font discovery mechanism, and try again."
        )?;
        stderr.flush()?;
        return Ok(1);
    }

    let mut stdout = BufWriter::with_capacity(2048, io::stdout().lock());

    // We'll be putting our fonts into a list categorized by family
    // so it is easier to read the output.
    let mut families: Vec<String> = Vec::new();
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    // Look up all available fonts. The library is only used by backends
    // that need it (the Windows backend opens candidate font files with
    // FreeType); other backends ignore it.
    let mut font_lib = font::Library::init().map_err(|e| format!("{:?}", e))?;
    let result = (|| -> RunResult {
        let disco = font::Discover::init(&font_lib);
        let mut disco_it = disco.discover(&font::Descriptor {
            family: config.family.as_deref().map(str::to_owned),
            style: config.style.as_deref().map(str::to_owned),
            bold: config.bold,
            italic: config.italic,
            monospace: config.family.is_none(),
            ..Default::default()
        })?;
        while let Some(face) = disco_it.next()? {
            let mut buf = [0u8; 1024];

            let family = match face.family_name(&mut buf) {
                Ok(family) => String::from_utf8_lossy(family).into_owned(),
                Err(err) => {
                    log::error!(target: LOG, "failed to get font family name: {}", err);
                    continue;
                }
            };

            let full_name = match face.name(&mut buf) {
                Ok(name) => String::from_utf8_lossy(name).into_owned(),
                Err(err) => {
                    log::error!(target: LOG, "failed to get font name: {}", err);
                    continue;
                }
            };

            let list = map.entry(family.clone()).or_insert_with(|| {
                families.push(family);
                Vec::new()
            });
            list.push(full_name);
        }
        Ok(0)
    })();
    font_lib.deinit();
    result?;

    // Sort our keys.
    if config.family.is_none() {
        families.sort_unstable_by(|l, r| l.as_bytes().cmp(r.as_bytes()));
    }

    // Output each
    for family in &families {
        let Some(list) = map.get_mut(family) else {
            continue;
        };
        if list.is_empty() {
            continue;
        }
        if config.family.is_none() {
            list.sort_unstable_by(|l, r| l.as_bytes().cmp(r.as_bytes()));
        }

        writeln!(stdout, "{}", family)?;
        for item in list.iter() {
            writeln!(stdout, "  {}", item)?;
        }
        writeln!(stdout)?;
    }

    stdout.flush()?;
    Ok(0)
}

/// Whether Ghostty was built with a font discovery mechanism (Zig
/// `font.Discover != void`).
pub(crate) const DISCOVERY_AVAILABLE: bool = !cfg!(target_arch = "wasm32");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_monospace_fonts() {
        let mut lib = font::Library::init().unwrap();
        {
            let disco = font::Discover::init(&lib);
            let mut it = disco
                .discover(&font::Descriptor {
                    monospace: true,
                    ..Default::default()
                })
                .unwrap();
            // The test environment may have no fonts; names must be
            // readable when there are.
            if let Some(face) = it.next().unwrap() {
                let mut buf = [0u8; 1024];
                assert!(face.family_name(&mut buf).is_ok());
            }
        }
        lib.deinit();
    }
}
