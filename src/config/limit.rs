//! Port of `src/config/limit.zig`.
//!
//! STATUS: ported

use std::fmt;

use crate::cli::args::{parse_int, Error, ParseCli};
use crate::config::formatter::{self, EntryFormatter, FormatEntry};

/// Integer types usable with [`Limit`] (Zig: `@typeInfo(T) == .int`).
pub trait LimitInt: Copy + PartialEq + fmt::Debug + TryFrom<i128> + FormatEntry {
    /// `std.math.maxInt(T)`
    const MAX: Self;
}

macro_rules! impl_limit_int {
    ($($t:ty),*) => {$(
        impl LimitInt for $t {
            const MAX: Self = <$t>::MAX;
        }
    )*};
}
impl_limit_int!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

/// A configurable integer limit with a declared default and support for the
/// special value `unlimited`.
///
/// Unlimited is stored as the maximum value of the integer type so the value
/// remains directly comparable without optional unwrapping.
///
/// The default value is a const generic `i128` because Rust doesn't allow
/// const parameters of a generic type: Zig `Limit(usize, 42)` is
/// `Limit<usize, 42>`, `Limit(usize, std.math.maxInt(usize))` is
/// `Limit<usize, { usize::MAX as i128 }>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit<T: LimitInt, const DEFAULT_VALUE: i128> {
    pub value: T,
}

impl<T: LimitInt, const DEFAULT_VALUE: i128> Default for Limit<T, DEFAULT_VALUE> {
    /// The limit initialized with its declared default value (Zig
    /// `Limit.default`).
    fn default() -> Self {
        let value = match T::try_from(DEFAULT_VALUE) {
            Ok(v) => v,
            Err(_) => panic!("Limit default value out of range"),
        };
        Limit { value }
    }
}

impl<T: LimitInt, const DEFAULT_VALUE: i128> Limit<T, DEFAULT_VALUE> {
    /// Returns the configured value, or null for the unlimited sentinel.
    pub fn optional(self) -> Option<T> {
        if self.value == T::MAX {
            return None;
        }
        Some(self.value)
    }
}

impl<T: LimitInt, const DEFAULT_VALUE: i128> ParseCli for Limit<T, DEFAULT_VALUE> {
    /// Parses an integer limit or the special value "unlimited".
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        let input = input.ok_or(Error::ValueRequired)?;
        if input == "unlimited" {
            return Ok(Limit { value: T::MAX });
        }

        Ok(Limit {
            value: parse_int::<T>(input, 0).map_err(|_| Error::InvalidValue)?,
        })
    }
}

impl<T: LimitInt, const DEFAULT_VALUE: i128> FormatEntry for Limit<T, DEFAULT_VALUE> {
    /// Formats the maximum integer value as "unlimited" and all other
    /// values as integers.
    fn format_entry(&self, formatter: &mut EntryFormatter<'_>) -> Result<(), formatter::Error> {
        if self.value == T::MAX {
            formatter.format_entry("unlimited")
        } else {
            formatter.format_entry(&self.value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::formatter::entry_formatter;

    #[test]
    fn limit_default_and_parsing() {
        type TestLimit = Limit<usize, 42>;

        assert_eq!(42usize, TestLimit::default().value);
        assert_eq!(123usize, TestLimit::parse_cli_new(Some("123")).unwrap().value);
        assert_eq!(
            usize::MAX,
            TestLimit::parse_cli_new(Some("unlimited")).unwrap().value
        );
        assert_eq!(Err(Error::InvalidValue), TestLimit::parse_cli_new(Some("invalid")));
    }

    #[test]
    fn limit_optional() {
        type TestLimit = Limit<u16, 42>;

        assert_eq!(Some(42u16), TestLimit::default().optional());
        assert_eq!(None, TestLimit { value: u16::MAX }.optional());
    }

    #[test]
    fn limit_formatting() {
        type TestLimit = Limit<usize, 42>;

        {
            let mut buf = String::new();
            (TestLimit { value: 123 })
                .format_entry(&mut entry_formatter("limit", &mut buf))
                .unwrap();
            assert_eq!("limit = 123\n", buf);
        }

        {
            let mut buf = String::new();
            (TestLimit { value: usize::MAX })
                .format_entry(&mut entry_formatter("limit", &mut buf))
                .unwrap();
            assert_eq!("limit = unlimited\n", buf);
        }
    }

    #[test]
    fn limit_max_default() {
        type Unlimited = Limit<usize, { usize::MAX as i128 }>;
        assert_eq!(None, Unlimited::default().optional());
    }
}
