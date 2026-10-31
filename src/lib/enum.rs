//! Port of `src/lib/enum.zig`.
//!
//! STATUS: ported
//!
//! # Usage for later ports
//!
//! Zig's `lib.Enum(target, keys)` builds an enum type at comptime whose
//! integer values are the index of each key in `keys`, where a `null` key
//! leaves an integer hole. The Rust equivalent is the [`lib_enum!`] macro:
//!
//! ```ignore
//! crate::lib_enum! {
//!     /// Docs for the enum.
//!     pub enum Shape: c {
//!         Default,   // = 0
//!         Pointer,   // = 1
//!         _,         // hole: 2 is reserved (was a removed key)
//!         Text,      // = 3
//!     }
//! }
//! ```
//!
//! * `c` produces a `#[repr(C)]` enum backed by `c_int` (the Zig `.c`
//!   target). Use it for every enum that crosses the C ABI.
//! * `zig` produces a `#[repr(u8)]` enum; [`LibEnum::BACKING_BITS`] reports
//!   the smallest unsigned bit width that can hold every key index
//!   (`std.math.IntFittingRange(0, keys.len - 1)` in Zig), holes included.
//!
//! The order of keys MUST NOT change when the values are part of an ABI or
//! a serialized format; remove a key by replacing it with `_`.
//!
//! The generated type implements [`LibEnum`], which gives reflection
//! (names, `@backingInt`, `@fromBackingInt`) used by
//! [`check_ghostty_h_enum`] and by the tagged union helpers in
//! [`crate::lib::union`].

use std::collections::HashMap;
use std::ffi::c_int;

use super::target::Target;

/// Reflection implemented by every enum generated with [`lib_enum!`].
pub trait LibEnum: Sized + Copy + Eq + 'static {
    /// The backing integer type (`c_int` for the C target, `u8` for Zig).
    type Backing: Copy + Eq + std::fmt::Debug;

    /// The target this enum was generated for.
    const TARGET: Target;

    /// Total number of keys, including holes.
    const KEYS_LEN: usize;

    /// Bit width of the backing integer. For the C target this is the
    /// width of `c_int`; for the Zig target it is the smallest width that
    /// can represent every key index including holes.
    const BACKING_BITS: u32;

    /// All present (non-hole) keys with their names, in declaration order.
    const FIELDS: &'static [(&'static str, Self)];

    /// The integer value of this tag (`@backingInt`).
    fn backing(self) -> Self::Backing;

    /// The integer value of this tag widened to `i64`.
    fn backing_i64(self) -> i64;

    /// Convert an integer value back into a tag (`@fromBackingInt`).
    /// Returns `None` for holes and out-of-range values.
    fn from_backing(v: Self::Backing) -> Option<Self>;

    /// The key name as declared (`@tagName`).
    fn tag_name(self) -> &'static str {
        for (name, value) in Self::FIELDS {
            if *value == self {
                return name;
            }
        }
        unreachable!()
    }
}

/// Number of bits required to hold every value in `0..=max`, matching
/// Zig's `std.math.IntFittingRange(0, max)`.
pub const fn int_fitting_range_bits(max: usize) -> u32 {
    usize::BITS - max.leading_zeros()
}

/// Create an enum type with the given keys that is C ABI compatible
/// if we're targeting C (`c`), otherwise a Rust enum with the smallest
/// possible backing type (`zig`).
///
/// In all cases, each enum value is its index in the key list. A `_` key
/// removes that field while preserving its integer hole. See the module
/// documentation for an example.
#[macro_export]
macro_rules! lib_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident : $target:ident { $($body:tt)* }
    ) => {
        $crate::lib_enum!(@munch
            meta [$(#[$meta])*] vis [$vis] name [$name] target [$target]
            n [0usize] acc [] rest [$($body)*]
        );
    };

    // A hole: skip the index.
    (@munch meta $m:tt vis [$vis:vis] name [$name:ident] target [$t:ident]
        n [$n:expr] acc [$($acc:tt)*] rest [_ $(, $($rest:tt)*)?]) => {
        $crate::lib_enum!(@munch meta $m vis [$vis] name [$name] target [$t]
            n [$n + 1usize] acc [$($acc)*] rest [$($($rest)*)?]);
    };

    // A key.
    (@munch meta $m:tt vis [$vis:vis] name [$name:ident] target [$t:ident]
        n [$n:expr] acc [$($acc:tt)*]
        rest [$(#[$km:meta])* $key:ident $(, $($rest:tt)*)?]) => {
        $crate::lib_enum!(@munch meta $m vis [$vis] name [$name] target [$t]
            n [$n + 1usize] acc [$($acc)* { [$(#[$km])*] $key [$n] }]
            rest [$($($rest)*)?]);
    };

    // Done: C target.
    (@munch meta [$($m:tt)*] vis [$vis:vis] name [$name:ident] target [c]
        n [$n:expr] acc [$({ [$($km:tt)*] $key:ident [$idx:expr] })*] rest []) => {
        $($m)*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(C)]
        $vis enum $name {
            $($($km)* $key = ($idx) as isize,)*
        }

        impl $crate::lib::r#enum::LibEnum for $name {
            type Backing = ::std::ffi::c_int;
            const TARGET: $crate::lib::target::Target = $crate::lib::target::Target::C;
            const KEYS_LEN: usize = $n;
            const BACKING_BITS: u32 = ::std::ffi::c_int::BITS;
            const FIELDS: &'static [(&'static str, Self)] =
                &[$((stringify!($key), $name::$key),)*];

            fn backing(self) -> ::std::ffi::c_int {
                self as ::std::ffi::c_int
            }

            fn backing_i64(self) -> i64 {
                self as ::std::ffi::c_int as i64
            }

            fn from_backing(v: ::std::ffi::c_int) -> Option<Self> {
                $(if v == $name::$key as ::std::ffi::c_int {
                    return Some($name::$key);
                })*
                None
            }
        }

        impl $crate::lib::union::CRepr for $name {
            type C = $name;
            fn cval(&self) -> $name {
                *self
            }
        }
    };

    // Done: Zig target.
    (@munch meta [$($m:tt)*] vis [$vis:vis] name [$name:ident] target [zig]
        n [$n:expr] acc [$({ [$($km:tt)*] $key:ident [$idx:expr] })*] rest []) => {
        $($m)*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        $vis enum $name {
            $($($km)* $key = ($idx) as u8,)*
        }

        const _: () = assert!(
            ($n) <= 256,
            "lib_enum! zig target supports at most 256 keys"
        );

        impl $crate::lib::r#enum::LibEnum for $name {
            type Backing = u8;
            const TARGET: $crate::lib::target::Target = $crate::lib::target::Target::Zig;
            const KEYS_LEN: usize = $n;
            const BACKING_BITS: u32 = $crate::lib::r#enum::int_fitting_range_bits(($n) - 1);
            const FIELDS: &'static [(&'static str, Self)] =
                &[$((stringify!($key), $name::$key),)*];

            fn backing(self) -> u8 {
                self as u8
            }

            fn backing_i64(self) -> i64 {
                self as u8 as i64
            }

            fn from_backing(v: u8) -> Option<Self> {
                $(if v == $name::$key as u8 {
                    return Some($name::$key);
                })*
                None
            }
        }
    };
}

/// Convert a Rust enum key name to the SCREAMING_SNAKE_CASE suffix used
/// by `ghostty.h`. Zig keys are `snake_case` so Zig simply upper-cases
/// them; Rust keys are usually `CamelCase` so word boundaries become `_`.
pub fn header_suffix(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let bytes = name.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = bytes[i - 1];
            let next_lower = bytes.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                out.push('_');
            }
        }
        out.push(c.to_ascii_uppercase() as char);
    }
    out
}

/// Verify that for every key in enum T, there is a matching declaration in
/// `ghostty.h` with the correct value. This should only ever be called
/// inside a test.
///
/// Zig imports `ghostty.h` with `@cImport`; the Rust port instead takes
/// the header source text (e.g. via `include_str!`) and extracts integer
/// constants (enum constants and `#define`s) with [`parse_c_constants`].
pub fn check_ghostty_h_enum<T: LibEnum>(prefix: &str, header_src: &str) -> Result<(), String> {
    if T::TARGET != Target::C {
        return Err("enum is not a C enum".to_string());
    }
    if T::BACKING_BITS != c_int::BITS {
        return Err("enum is not backed by c_int".to_string());
    }

    let c = parse_c_constants(header_src);

    // Every key starts in the set; matched keys are removed.
    let mut missing: Vec<&'static str> = T::FIELDS.iter().map(|(n, _)| *n).collect();

    for (field, value) in T::FIELDS {
        let expected_name = format!("{}{}", prefix, header_suffix(field));
        if let Some(&header_value) = c.get(&expected_name) {
            if header_value != value.backing_i64() {
                log::error!(
                    "{} key {} does not have the same backing int as {}",
                    std::any::type_name::<T>(),
                    field,
                    expected_name
                );
                return Err(format!(
                    "{field}: expected {expected_name} = {}, header has {header_value}",
                    value.backing_i64()
                ));
            }
            missing.retain(|m| m != field);
        }
    }

    if !missing.is_empty() {
        for m in &missing {
            log::error!(
                "ghostty.h is missing value for {}{}",
                prefix,
                header_suffix(m)
            );
        }
        return Err(format!("ghostty.h is missing values for {missing:?}"));
    }

    Ok(())
}

/// Extract integer constants from C source: every enumerator in an `enum`
/// body (with implicit increments) and every `#define NAME <int-expr>`.
/// Supported expressions: integer literals (decimal/hex/octal with
/// `u`/`l` suffixes), previously seen names, unary `-`/`~`, `<<`, `>>`,
/// `|`, `&`, `+`, `-`, and parentheses.
pub fn parse_c_constants(src: &str) -> HashMap<String, i64> {
    let src = strip_c_comments(src);
    let mut out: HashMap<String, i64> = HashMap::new();

    // #defines
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('#') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("define") else {
            continue;
        };
        let rest = rest.trim_start();
        let name_end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let (name, value) = rest.split_at(name_end);
        if name.is_empty() || value.starts_with('(') {
            continue;
        }
        if let Some(v) = eval_c_expr(value.trim(), &out) {
            out.insert(name.to_string(), v);
        }
    }

    // enum bodies
    let bytes = src.as_bytes();
    let mut i = 0;
    while let Some(pos) = src[i..].find("enum") {
        let start = i + pos;
        i = start + 4;
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = i >= bytes.len() || !is_ident_byte(bytes[i]);
        if !before_ok || !after_ok {
            continue;
        }

        // Skip an optional tag name, then expect `{`.
        let mut j = i;
        while j < bytes.len() && (bytes[j].is_ascii_whitespace() || is_ident_byte(bytes[j])) {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'{' {
            continue;
        }
        let Some(close) = src[j..].find('}') else {
            break;
        };
        let body = &src[j + 1..j + close];
        i = j + close + 1;

        let mut next: i64 = 0;
        for entry in body.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (name, value) = match entry.split_once('=') {
                Some((n, v)) => match eval_c_expr(v.trim(), &out) {
                    Some(v) => (n.trim(), v),
                    None => continue,
                },
                None => (entry, next),
            };
            out.insert(name.to_string(), value);
            next = value.wrapping_add(1);
        }
    }

    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn strip_c_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    loop {
        let block = rest.find("/*");
        let line = rest.find("//");
        match (block, line) {
            (None, None) => {
                out.push_str(rest);
                break;
            }
            (Some(b), l) if l.is_none_or(|l| b < l) => {
                out.push_str(&rest[..b]);
                match rest[b + 2..].find("*/") {
                    Some(e) => rest = &rest[b + 2 + e + 2..],
                    None => break,
                }
            }
            (_, Some(l)) => {
                out.push_str(&rest[..l]);
                match rest[l..].find('\n') {
                    Some(e) => rest = &rest[l + e..],
                    None => break,
                }
            }
            _ => unreachable!(),
        }
    }
    out
}

/// Evaluate a small C integer constant expression.
fn eval_c_expr(expr: &str, names: &HashMap<String, i64>) -> Option<i64> {
    let tokens = tokenize_c_expr(expr)?;
    let mut p = ExprParser {
        tokens: &tokens,
        pos: 0,
        names,
    };
    let v = p.or()?;
    if p.pos != tokens.len() {
        return None;
    }
    Some(v)
}

#[derive(Debug, Clone, PartialEq)]
enum CTok {
    Num(i64),
    Ident(String),
    Op(&'static str),
}

fn tokenize_c_expr(expr: &str) -> Option<Vec<CTok>> {
    let b = expr.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let lit = expr[start..i].trim_end_matches(['u', 'U', 'l', 'L']);
            let v = if let Some(hex) = lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
                i64::from_str_radix(hex, 16).ok()?
            } else if lit.len() > 1 && lit.starts_with('0') {
                i64::from_str_radix(&lit[1..], 8).ok()?
            } else {
                lit.parse::<i64>().ok()?
            };
            out.push(CTok::Num(v));
        } else if is_ident_byte(c) {
            let start = i;
            while i < b.len() && is_ident_byte(b[i]) {
                i += 1;
            }
            out.push(CTok::Ident(expr[start..i].to_string()));
        } else {
            let two = expr.get(i..i + 2);
            let op: &'static str = match two {
                Some("<<") => "<<",
                Some(">>") => ">>",
                _ => match c {
                    b'|' => "|",
                    b'&' => "&",
                    b'+' => "+",
                    b'-' => "-",
                    b'~' => "~",
                    b'(' => "(",
                    b')' => ")",
                    _ => return None,
                },
            };
            i += op.len();
            out.push(CTok::Op(op));
        }
    }
    Some(out)
}

struct ExprParser<'a> {
    tokens: &'a [CTok],
    pos: usize,
    names: &'a HashMap<String, i64>,
}

impl ExprParser<'_> {
    fn peek_op(&self, op: &str) -> bool {
        matches!(self.tokens.get(self.pos), Some(CTok::Op(o)) if *o == op)
    }

    fn or(&mut self) -> Option<i64> {
        let mut v = self.and()?;
        while self.peek_op("|") {
            self.pos += 1;
            v |= self.and()?;
        }
        Some(v)
    }

    fn and(&mut self) -> Option<i64> {
        let mut v = self.shift()?;
        while self.peek_op("&") {
            self.pos += 1;
            v &= self.shift()?;
        }
        Some(v)
    }

    fn shift(&mut self) -> Option<i64> {
        let mut v = self.add()?;
        loop {
            if self.peek_op("<<") {
                self.pos += 1;
                v = v.checked_shl(self.add()? as u32)?;
            } else if self.peek_op(">>") {
                self.pos += 1;
                v = v.checked_shr(self.add()? as u32)?;
            } else {
                return Some(v);
            }
        }
    }

    fn add(&mut self) -> Option<i64> {
        let mut v = self.unary()?;
        loop {
            if self.peek_op("+") {
                self.pos += 1;
                v = v.wrapping_add(self.unary()?);
            } else if self.peek_op("-") {
                self.pos += 1;
                v = v.wrapping_sub(self.unary()?);
            } else {
                return Some(v);
            }
        }
    }

    fn unary(&mut self) -> Option<i64> {
        match self.tokens.get(self.pos)? {
            CTok::Op("-") => {
                self.pos += 1;
                Some(self.unary()?.wrapping_neg())
            }
            CTok::Op("~") => {
                self.pos += 1;
                Some(!self.unary()?)
            }
            CTok::Op("(") => {
                self.pos += 1;
                let v = self.or()?;
                if !self.peek_op(")") {
                    return None;
                }
                self.pos += 1;
                Some(v)
            }
            CTok::Num(n) => {
                self.pos += 1;
                Some(*n)
            }
            CTok::Ident(name) => {
                self.pos += 1;
                self.names.get(name).copied()
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib_enum;

    #[test]
    fn zig() {
        lib_enum! { enum T: zig { A, B, C, D } }
        assert_eq!(2, T::BACKING_BITS);
        assert_eq!(1, std::mem::size_of::<T>());
    }

    #[test]
    fn c() {
        lib_enum! { enum T: c { A, B, C, D } }
        assert_eq!(<T as LibEnum>::TARGET, Target::C);
        assert_eq!(c_int::BITS, T::BACKING_BITS);
        assert_eq!(std::mem::size_of::<c_int>(), std::mem::size_of::<T>());
    }

    #[test]
    fn stable_values_when_removing_a_key() {
        // C
        {
            lib_enum! { enum T: c { A, B, _, D } }
            assert_eq!(c_int::BITS, T::BACKING_BITS);
            assert_eq!(3, T::D.backing());
            assert_eq!(None, T::from_backing(2));
            assert_eq!(Some(T::D), T::from_backing(3));
        }

        // Zig
        {
            lib_enum! { enum T: zig { A, B, _, D } }
            assert_eq!(2, T::BACKING_BITS);
            assert_eq!(3, T::D.backing());
        }
    }

    #[test]
    fn zig_backing_integer_includes_trailing_holes() {
        lib_enum! { enum T: zig { A, _, _, _, _ } }
        assert_eq!(3, T::BACKING_BITS);
        assert_eq!(0, T::A.backing());
        assert_eq!(5, T::KEYS_LEN);
    }

    #[test]
    fn zig_values_remain_stable_across_multiple_holes() {
        lib_enum! { enum T: zig { _, B, _, D, _, F } }
        assert_eq!(3, T::BACKING_BITS);
        assert_eq!(1, T::B.backing());
        assert_eq!(3, T::D.backing());
        assert_eq!(5, T::F.backing());
        assert_eq!("D", T::D.tag_name());
    }

    #[test]
    fn check_ghostty_h_enum_header() {
        lib_enum! { enum Shape: c { Default, ContextMenu, _, Text } }
        let header = r#"
            /* comment GHOSTTY_SHAPE_BOGUS = 9 */
            typedef enum {
              GHOSTTY_SHAPE_DEFAULT, // implicit 0
              GHOSTTY_SHAPE_CONTEXT_MENU,
              GHOSTTY_SHAPE_TEXT = 3,
            } ghostty_shape_e;
        "#;
        check_ghostty_h_enum::<Shape>("GHOSTTY_SHAPE_", header).unwrap();

        // Wrong value.
        let bad = "enum { GHOSTTY_SHAPE_DEFAULT, GHOSTTY_SHAPE_CONTEXT_MENU, GHOSTTY_SHAPE_TEXT };";
        assert!(check_ghostty_h_enum::<Shape>("GHOSTTY_SHAPE_", bad).is_err());

        // Missing value.
        let missing = "enum { GHOSTTY_SHAPE_DEFAULT, GHOSTTY_SHAPE_CONTEXT_MENU };";
        assert!(check_ghostty_h_enum::<Shape>("GHOSTTY_SHAPE_", missing).is_err());
    }

    #[test]
    fn parse_c_constants_expressions() {
        let c = parse_c_constants(
            "#define GHOSTTY_X 0x10\nenum { A = 1 << 2, B, C = A | 1, D = -1, E };",
        );
        assert_eq!(c["GHOSTTY_X"], 16);
        assert_eq!(c["A"], 4);
        assert_eq!(c["B"], 5);
        assert_eq!(c["C"], 5);
        assert_eq!(c["D"], -1);
        assert_eq!(c["E"], 0);
        assert_eq!(header_suffix("ContextMenu"), "CONTEXT_MENU");
        assert_eq!(header_suffix("snake_case"), "SNAKE_CASE");
        assert_eq!(header_suffix("URLOpen"), "URL_OPEN");
    }
}
