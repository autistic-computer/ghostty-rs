//! Port of `src/config/io.zig`.
//!
//! STATUS: ported

use crate::cli::args::{
    self, cli_union, parse_cli_new_from_default, Error, ParseCli, StringZ, TaggedUnion,
};
use crate::config::formatter::{self, EntryFormatter, FormatEntry};
use crate::config::string;

cli_union! {
    @custom_parse
    /// ReadableIO is some kind of IO source that is readable.
    ///
    /// It can be either a direct string or a filepath. The filepath will
    /// be deferred and read later, so it won't be checked for existence
    /// or readability at configuration time. This allows using a path that
    /// might be produced in an intermediate state.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ReadableIO {
        Raw(StringZ) => "raw",
        Path(StringZ) => "path",
    }
}

impl ParseCli for ReadableIO {
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        let input = input.ok_or(Error::ValueRequired)?;
        if input.is_empty() {
            return Err(Error::ValueRequired);
        }

        // We create a buffer only to do string parsing and validate
        // it works. We store the value as raw so that our formatting
        // can recreate it.
        {
            let mut buf = vec![0u8; input.len()];
            string::parse(&mut buf, input.as_bytes())?;
        }

        // Next, parse the tagged union using normal rules.
        match args::parse_tagged_union::<Self>(input) {
            Ok(v) => Ok(v),
            // Invalid values in the tagged union are interpreted as
            // raw values. This lets users pass in simple string values
            // without needing to tag them.
            Err(Error::InvalidValue) => Ok(ReadableIO::Raw(StringZ::from(input))),
            Err(err) => Err(err),
        }
    }
}

impl ReadableIO {
    /// The stored (unparsed) value.
    fn value(&self) -> &StringZ {
        match self {
            ReadableIO::Raw(v) | ReadableIO::Path(v) => v,
        }
    }

    /// Same as clone but also parses the values as Zig strings in
    /// the final resulting value.
    pub fn clone_parsed(&self) -> ReadableIO {
        let v = self.value();
        // Parsing can't fail because we validate it in parseCLI
        let mut copied = v.as_bytes().to_vec();
        let len = string::parse_in_place(&mut copied).expect("validated in parse_cli");
        copied.truncate(len);
        // `\xNN` escapes may produce invalid UTF-8; we store it lossily.
        let parsed = StringZ::from(String::from_utf8_lossy(&copied).into_owned());

        match self {
            ReadableIO::Raw(_) => ReadableIO::Raw(parsed),
            ReadableIO::Path(_) => ReadableIO::Path(parsed),
        }
    }

    pub fn equal(&self, other: &ReadableIO) -> bool {
        self == other
    }
}

impl FormatEntry for ReadableIO {
    fn format_entry(&self, formatter: &mut EntryFormatter<'_>) -> Result<(), formatter::Error> {
        // Zig formats into a fixed 4096 byte buffer.
        const BUF_LEN: usize = 4096;
        let buf = format!("{}:{}", self.tag_name(), self.value());
        if buf.len() > BUF_LEN {
            return Err(formatter::Error::OutOfMemory);
        }

        formatter.format_entry(buf.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepeatableReadableIO {
    pub list: Vec<ReadableIO>,
}

impl ParseCli for RepeatableReadableIO {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let value = input.ok_or(Error::ValueRequired)?;

        // Empty value resets the list
        if value.is_empty() {
            self.list.clear();
            return Ok(());
        }

        let io = ReadableIO::parse_cli_new(Some(value))?;
        self.list.push(io);
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        parse_cli_new_from_default(input)
    }
}

impl RepeatableReadableIO {
    /// See ReadableIO.cloneParsed
    pub fn clone_parsed(&self) -> RepeatableReadableIO {
        RepeatableReadableIO {
            list: self.list.iter().map(ReadableIO::clone_parsed).collect(),
        }
    }

    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(&self, other: &RepeatableReadableIO) -> bool {
        if self.list.len() != other.list.len() {
            return false;
        }
        self.list.iter().zip(other.list.iter()).all(|(a, b)| a.equal(b))
    }
}

impl FormatEntry for RepeatableReadableIO {
    /// Used by Formatter
    fn format_entry(&self, formatter: &mut EntryFormatter<'_>) -> Result<(), formatter::Error> {
        if self.list.is_empty() {
            return formatter.format_entry(&());
        }

        for value in &self.list {
            formatter.format_entry(value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::formatter::entry_formatter;

    #[test]
    fn readable_io_parse_cli() {
        {
            let io = ReadableIO::parse_cli_new(Some("foo")).unwrap();
            assert_eq!(ReadableIO::Raw("foo".into()), io);
        }
        {
            let io = ReadableIO::parse_cli_new(Some("raw:foo")).unwrap();
            assert_eq!(ReadableIO::Raw("foo".into()), io);
        }
        {
            let io = ReadableIO::parse_cli_new(Some("path:foo")).unwrap();
            assert_eq!(ReadableIO::Path("foo".into()), io);
        }
    }

    #[test]
    fn readable_io_parse_cli_errors() {
        assert_eq!(Err(Error::ValueRequired), ReadableIO::parse_cli_new(None));
        assert_eq!(Err(Error::ValueRequired), ReadableIO::parse_cli_new(Some("")));
        assert_eq!(
            Err(Error::Other("InvalidString")),
            ReadableIO::parse_cli_new(Some("raw:\\q"))
        );
    }

    #[test]
    fn readable_io_format_entry() {
        let mut buf = String::new();

        let v = ReadableIO::parse_cli_new(Some("raw:foo")).unwrap();
        v.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = raw:foo\n", buf);
    }

    #[test]
    fn readable_io_clone_parsed() {
        let v = ReadableIO::parse_cli_new(Some("raw:a\\nb")).unwrap();
        assert_eq!(ReadableIO::Raw("a\\nb".into()), v);
        assert_eq!(ReadableIO::Raw("a\nb".into()), v.clone_parsed());
    }

    #[test]
    fn repeatable_readable_io_parse_cli() {
        let mut list = RepeatableReadableIO::default();
        list.parse_cli(Some("raw:A")).unwrap();
        list.parse_cli(Some("path:B")).unwrap();
        assert_eq!(2, list.list.len());

        list.parse_cli(Some("")).unwrap();
        assert_eq!(0, list.list.len());
    }

    #[test]
    fn repeatable_readable_io_format_and_equal() {
        let mut list = RepeatableReadableIO::default();
        {
            let mut buf = String::new();
            list.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
            assert_eq!("a = \n", buf);
        }
        list.parse_cli(Some("raw:A")).unwrap();
        list.parse_cli(Some("path:B")).unwrap();
        {
            let mut buf = String::new();
            list.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
            assert_eq!("a = raw:A\na = path:B\n", buf);
        }
        assert!(list.equal(&list.clone()));
        assert!(list.equal(&list.clone_parsed()));
        assert!(!list.equal(&RepeatableReadableIO::default()));
    }
}
