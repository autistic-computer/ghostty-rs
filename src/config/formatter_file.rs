//! Port of `src/config/formatter_file.zig`.
//!
//! STATUS: ported
//!
//! IMPORTANT: This is in a separate file from formatter.zig because it
//! puts a build-time dependency on Config.zig which brings in too much
//! into libghostty-vt tests which reference some formattable types.

use std::fmt;

use super::config::Config;
use super::key::Key;
use crate::cli::args::{Fields, FormatFields};

/// FileFormatter is a formatter implementation that outputs the
/// config in a file-like format. This uses more generous whitespace,
/// can include comments, etc.
#[derive(Clone, Copy)]
pub struct FileFormatter<'a> {
    pub config: &'a Config,

    /// Include comments for documentation of each key
    pub docs: bool,

    /// Only include changed values from the default.
    pub changed: bool,
}

impl<'a> FileFormatter<'a> {
    pub fn new(config: &'a Config) -> Self {
        FileFormatter {
            config,
            docs: false,
            changed: false,
        }
    }

    /// Implements std.fmt so it can be used directly with std.fmt.
    pub fn format(&self, writer: &mut dyn fmt::Write) -> fmt::Result {
        // If we're change-tracking then we need the default config to
        // compare against.
        let default: Option<Config> = if self.changed { Some(Config::default()) } else { None };

        for (i, field) in Config::FIELDS.iter().enumerate() {
            let do_format = match &default {
                Some(d) => d.changed(self.config, Key::from_index(i)),
                None => true,
            };

            if do_format {
                let do_docs = self.docs && field.has_help();
                if do_docs {
                    let help = field.help();
                    for line in help.split('\n') {
                        write!(writer, "# {}\n", line)?;
                    }
                }

                self.config.format_field(i, writer).map_err(|_| fmt::Error)?;

                if do_docs {
                    write!(writer, "\n")?;
                }
            }
        }

        Ok(())
    }
}

impl fmt::Display for FileFormatter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_default_config() {
        let cfg = Config::default();

        let mut buf = String::new();

        // We just make sure this works without errors. We aren't asserting output.
        let fmt = FileFormatter::new(&cfg);
        fmt.format(&mut buf).unwrap();
    }

    #[test]
    fn format_default_config_changed() {
        let mut cfg = Config::default();
        cfg.font_size = 42.0;

        let mut buf = String::new();

        // We just make sure this works without errors. We aren't asserting output.
        let fmt = FileFormatter {
            config: &cfg,
            docs: false,
            changed: true,
        };
        fmt.format(&mut buf).unwrap();
        assert!(buf.contains("font-size = 42\n"));
    }
}
