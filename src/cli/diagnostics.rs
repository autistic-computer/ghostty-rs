//! Port of `src/cli/diagnostics.zig`.
//!
//! STATUS: ported

use std::fmt;

use super::args::{ArgIterator, StringZ};

/// A diagnostic message from parsing. This is used to provide additional
/// human-friendly warnings and errors about the parsed data.
///
/// In Zig all of the memory for the diagnostic is allocated from the arena
/// associated with the config structure. In Rust the diagnostic owns its
/// memory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diagnostic {
    pub location: Location,
    pub key: StringZ,
    pub message: StringZ,
}

impl Diagnostic {
    /// Write the full user-friendly diagnostic message to the writer.
    /// (Zig's `format`; also available through [`fmt::Display`].)
    pub fn format(&self, writer: &mut dyn fmt::Write) -> fmt::Result {
        match &self.location {
            Location::None => {}
            Location::Cli(index) => write!(writer, "cli:{}:", index)?,
            Location::File { path, line } => write!(writer, "{}:{}:", path, line)?,
        }

        if !self.key.is_empty() {
            write!(writer, "{}: ", self.key.as_str())?;
        } else if self.location != Location::None {
            write!(writer, " ")?;
        }

        write!(writer, "{}", self.message.as_str())
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
    }
}

/// The possible locations for a diagnostic message. This is used
/// to provide context for the message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Location {
    #[default]
    None,
    Cli(usize),
    File { path: String, line: usize },
}

/// The tag of a [`Location`] (Zig `Location.Key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocationKey {
    None,
    Cli,
    File,
}

impl Location {
    /// The active tag of this location.
    pub fn key(&self) -> LocationKey {
        match self {
            Location::None => LocationKey::None,
            Location::Cli(_) => LocationKey::Cli,
            Location::File { .. } => LocationKey::File,
        }
    }

    /// Returns the location reported by the iterator, or `none` if the
    /// iterator doesn't track locations.
    pub fn from_iter<I: ArgIterator + ?Sized>(iter: &I) -> Location {
        iter.location().unwrap_or(Location::None)
    }
}

/// Whether diagnostic messages are precomputed (Zig `precompute_enabled`).
///
/// We enable precompute for tests so that the logic is semantically
/// analyzed and run. We specifically want precompute for libghostty.
const PRECOMPUTE_ENABLED: bool = match crate::build_config::ARTIFACT {
    crate::build_config::Artifact::Exe | crate::build_config::Artifact::WasmModule => cfg!(test),
    crate::build_config::Artifact::Lib => true,
};

/// Precomputed data for diagnostics. This is used specifically
/// when we build libghostty so that we can precompute the messages
/// and return them via the C API without allocating memory at
/// call time. This is always present in Rust but only populated
/// when precompute is enabled (in Zig it is `void` otherwise).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Precompute {
    pub messages: Vec<StringZ>,
}

/// A list of diagnostics. The `_diagnostics` field must be this type
/// for diagnostics to be supported (see
/// [`ParseHooks::diagnostics_mut`](super::args::ParseHooks::diagnostics_mut)).
///
/// This is implemented as a simple wrapper around an array list
/// so that we can inject some logic around adding diagnostics
/// and potentially in the future structure them differently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticList {
    /// The list of diagnostics.
    pub list: Vec<Diagnostic>,

    /// Precomputed data for diagnostics.
    pub precompute: Precompute,
}

impl DiagnosticList {
    pub fn append(&mut self, diag: Diagnostic) {
        if PRECOMPUTE_ENABLED {
            let mut msg = String::new();
            // Writing to a String can't fail.
            let _ = diag.format(&mut msg);
            self.precompute.messages.push(StringZ::from(msg));
        }

        self.list.push(diag);

        if PRECOMPUTE_ENABLED {
            debug_assert_eq!(self.precompute.messages.len(), self.list.len());
        }
    }

    pub fn empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn items(&self) -> &[Diagnostic] {
        &self.list
    }

    /// Returns true if there are any diagnostics for the given
    /// location type.
    pub fn contains_location(&self, location: LocationKey) -> bool {
        self.list.iter().any(|diag| diag.location.key() == location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_format() {
        let d = Diagnostic {
            location: Location::File {
                path: "foo".into(),
                line: 3,
            },
            key: "a".into(),
            message: "bad".into(),
        };
        assert_eq!("foo:3:a: bad", d.to_string());

        let d = Diagnostic {
            location: Location::Cli(2),
            key: "".into(),
            message: "bad".into(),
        };
        assert_eq!("cli:2: bad", d.to_string());

        let d = Diagnostic {
            message: "bad".into(),
            ..Default::default()
        };
        assert_eq!("bad", d.to_string());
    }

    #[test]
    fn diagnostic_list_precompute() {
        let mut list = DiagnosticList::default();
        assert!(list.empty());
        list.append(Diagnostic {
            location: Location::Cli(1),
            key: "k".into(),
            message: "m".into(),
        });
        assert!(!list.empty());
        assert_eq!(1, list.items().len());
        assert_eq!(1, list.precompute.messages.len());
        assert_eq!("cli:1:k: m", list.precompute.messages[0].as_str());
        assert!(list.contains_location(LocationKey::Cli));
        assert!(!list.contains_location(LocationKey::File));

        let cloned = list.clone();
        assert_eq!(cloned, list);
    }
}
