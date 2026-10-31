//! Port of `src/input/key_mods.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io::Write as _;
use std::str::FromStr;

use indexmap::IndexMap;

use crate::config::formatter::{self, EntryFormatter};
use super::config::OptionAsAlt;

/// Aliases for modifier names.
pub const ALIAS: &[(&str, Mod)] = &[
    ("cmd", Mod::Super),
    ("command", Mod::Super),
    ("opt", Mod::Alt),
    ("option", Mod::Alt),
    ("control", Mod::Ctrl),
];

/// Single modifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mod {
    Shift,
    Ctrl,
    Alt,
    Super,
}

impl Mod {
    /// All modifiers in declaration order.
    pub const ALL: [Mod; 4] = [Mod::Shift, Mod::Ctrl, Mod::Alt, Mod::Super];

    /// The Zig tag name (`@tagName`), used in configuration strings.
    pub const fn as_str(self) -> &'static str {
        match self {
            Mod::Shift => "shift",
            Mod::Ctrl => "ctrl",
            Mod::Alt => "alt",
            Mod::Super => "super",
        }
    }

    /// Exact tag name match (`std.meta.stringToEnum`).
    pub fn parse(s: &[u8]) -> Option<Mod> {
        match s {
            b"shift" => Some(Mod::Shift),
            b"ctrl" => Some(Mod::Ctrl),
            b"alt" => Some(Mod::Alt),
            b"super" => Some(Mod::Super),
            _ => None,
        }
    }
}

impl FromStr for Mod {
    type Err = ();

    fn from_str(s: &str) -> Result<Mod, ()> {
        Mod::parse(s.as_bytes()).ok_or(())
    }
}

impl fmt::Display for Mod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `Mod.Side` in Zig (`enum(u1)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ModSide {
    #[default]
    Left = 0,
    Right = 1,
}

impl ModSide {
    /// The Zig tag name (`@tagName`).
    pub const fn as_str(self) -> &'static str {
        match self {
            ModSide::Left => "left",
            ModSide::Right => "right",
        }
    }

    /// Exact tag name match (`std.meta.stringToEnum`).
    pub fn parse(s: &[u8]) -> Option<ModSide> {
        match s {
            b"left" => Some(ModSide::Left),
            b"right" => Some(ModSide::Right),
            _ => None,
        }
    }

    const fn from_bit(b: u8) -> ModSide {
        if b & 1 == 0 {
            ModSide::Left
        } else {
            ModSide::Right
        }
    }
}

/// A bitmask for all key modifiers.
///
/// This is a `packed struct(u16)` in Zig; the bit layout is available via
/// [`Mods::int`] / [`Mods::from_int`]:
///
/// | bit | field |
/// | --- | --- |
/// | 0 | shift |
/// | 1 | ctrl |
/// | 2 | alt |
/// | 3 | super |
/// | 4 | caps_lock |
/// | 5 | num_lock |
/// | 6..10 | sides (shift, ctrl, alt, super) |
/// | 10..16 | padding (always zero) |
///
/// IMPORTANT: Any changes here update include/ghostty.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// `super` in Zig (a reserved word in Rust).
    pub super_: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub sides: ModsSide,
}

/// `Mods.Backing` in Zig.
pub type ModsBacking = u16;

/// The standard modifier keys only. Does not include the lock keys,
/// only standard bindable keys.
///
/// (Zig: `Mods.Keys`, a `packed struct(u4)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ModsKeys {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_: bool,
}

impl ModsKeys {
    /// The backing integer (a `u4` in Zig).
    #[inline]
    pub const fn int(self) -> u8 {
        (self.shift as u8)
            | ((self.ctrl as u8) << 1)
            | ((self.alt as u8) << 2)
            | ((self.super_ as u8) << 3)
    }

    /// Inverse of [`ModsKeys::int`]; only the low 4 bits are used.
    #[inline]
    pub const fn from_int(v: u8) -> ModsKeys {
        ModsKeys {
            shift: v & 0b0001 != 0,
            ctrl: v & 0b0010 != 0,
            alt: v & 0b0100 != 0,
            super_: v & 0b1000 != 0,
        }
    }
}

/// Tracks the side that is active for any given modifier. Note
/// that this doesn't confirm a modifier is pressed; you must check
/// the bool for that in addition to this.
///
/// Not all platforms support this, check apprt for more info.
///
/// (Zig: `Mods.Side`, a `packed struct(u4)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ModsSide {
    pub shift: ModSide,
    pub ctrl: ModSide,
    pub alt: ModSide,
    pub super_: ModSide,
}

impl ModsSide {
    /// The backing integer (a `u4` in Zig).
    #[inline]
    pub const fn int(self) -> u8 {
        (self.shift as u8)
            | ((self.ctrl as u8) << 1)
            | ((self.alt as u8) << 2)
            | ((self.super_ as u8) << 3)
    }

    /// Inverse of [`ModsSide::int`]; only the low 4 bits are used.
    #[inline]
    pub const fn from_int(v: u8) -> ModsSide {
        ModsSide {
            shift: ModSide::from_bit(v),
            ctrl: ModSide::from_bit(v >> 1),
            alt: ModSide::from_bit(v >> 2),
            super_: ModSide::from_bit(v >> 3),
        }
    }
}

impl Mods {
    /// No modifiers set (Zig: `Mods{}` / `.{}`).
    pub const EMPTY: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
        super_: false,
        caps_lock: false,
        num_lock: false,
        sides: ModsSide {
            shift: ModSide::Left,
            ctrl: ModSide::Left,
            alt: ModSide::Left,
            super_: ModSide::Left,
        },
    };

    /// The mask that has all the side bits set.
    pub const SIDE_MASK: Mods = Mods {
        sides: ModsSide {
            shift: ModSide::Right,
            ctrl: ModSide::Right,
            alt: ModSide::Right,
            super_: ModSide::Right,
        },
        ..Mods::EMPTY
    };

    /// Integer value of this struct.
    #[inline]
    pub const fn int(self) -> ModsBacking {
        (self.shift as u16)
            | ((self.ctrl as u16) << 1)
            | ((self.alt as u16) << 2)
            | ((self.super_ as u16) << 3)
            | ((self.caps_lock as u16) << 4)
            | ((self.num_lock as u16) << 5)
            | ((self.sides.int() as u16) << 6)
    }

    /// Inverse of [`Mods::int`] (`@bitCast`). Padding bits are ignored.
    #[inline]
    pub const fn from_int(v: ModsBacking) -> Mods {
        Mods {
            shift: v & (1 << 0) != 0,
            ctrl: v & (1 << 1) != 0,
            alt: v & (1 << 2) != 0,
            super_: v & (1 << 3) != 0,
            caps_lock: v & (1 << 4) != 0,
            num_lock: v & (1 << 5) != 0,
            sides: ModsSide::from_int(((v >> 6) & 0xF) as u8),
        }
    }

    /// Returns true if no modifiers are set.
    #[inline]
    pub const fn empty(self) -> bool {
        self.int() == 0
    }

    /// Returns true if two mods are equal.
    #[inline]
    pub const fn equal(self, other: Mods) -> bool {
        self.int() == other.int()
    }

    /// Returns only the keys.
    ///
    /// In the future I want to remove `binding` for this. I didn't want
    /// to do that all in one PR where I added this because its a bigger
    /// change.
    #[inline]
    pub const fn keys(self) -> ModsKeys {
        let backing: u8 = (self.int() & 0xF) as u8;
        ModsKeys::from_int(backing)
    }

    /// Return mods that are only relevant for bindings.
    #[inline]
    pub const fn binding(self) -> Mods {
        Mods {
            shift: self.shift,
            ctrl: self.ctrl,
            alt: self.alt,
            super_: self.super_,
            ..Mods::EMPTY
        }
    }

    /// Perform `self &~ other` to remove the other mods from self.
    #[inline]
    pub const fn unset(self, other: Mods) -> Mods {
        Mods::from_int(self.int() & !other.int())
    }

    /// Returns the mods without locks set.
    #[inline]
    pub const fn without_locks(self) -> Mods {
        let mut copy = self;
        copy.caps_lock = false;
        copy.num_lock = false;
        copy
    }

    /// Return the mods to use for key translation. This handles settings
    /// like macos-option-as-alt. The translation mods should be used for
    /// translation but never sent back in for the key callback.
    pub fn translation(self, option_as_alt: OptionAsAlt) -> Mods {
        let mut result = self;

        // macos-option-as-alt for darwin
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            'alt: {
                // Alt has to be set only on the correct side
                match option_as_alt {
                    OptionAsAlt::False => break 'alt,
                    OptionAsAlt::True => {}
                    OptionAsAlt::Left => {
                        if self.sides.alt == ModSide::Right {
                            break 'alt;
                        }
                    }
                    OptionAsAlt::Right => {
                        if self.sides.alt == ModSide::Left {
                            break 'alt;
                        }
                    }
                }

                // Unset alt
                result.alt = false;
            }
        }

        result
    }

    /// Checks to see if super is on (MacOS) or ctrl.
    #[inline]
    pub const fn ctrl_or_super(self) -> bool {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            return self.super_;
        }
        self.ctrl
    }
}

/// Errors from [`RemapSet::parse`]. (Allocation failure is not a
/// recoverable error in the Rust port.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    MissingAssignment,
    InvalidMod,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingAssignment => f.write_str("MissingAssignment"),
            ParseError::InvalidMod => f.write_str("InvalidMod"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Errors from [`RemapSet::parse_cli`] (the Zig inferred error set
/// `error{InvalidValue}`, minus `OutOfMemory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseCliError {
    InvalidValue,
}

impl fmt::Display for ParseCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InvalidValue")
    }
}

impl std::error::Error for ParseCliError {}

/// Modifier remapping. See `key-remap` in Config.zig for detailed docs.
#[derive(Debug, Clone, Default)]
pub struct RemapSet {
    /// Available mappings. (Zig: `std.AutoArrayHashMapUnmanaged(Mods, Mods)`,
    /// an insertion-ordered hash map.)
    pub map: IndexMap<Mods, Mods>,

    /// The mask of remapped modifiers that can be used to quickly
    /// check if some input mods need remapping.
    pub mask: Mask,
}

impl RemapSet {
    /// An empty set (Zig: `RemapSet.empty`).
    pub fn empty() -> RemapSet {
        RemapSet {
            map: IndexMap::new(),
            mask: Mask::default(),
        }
    }

    /// Parse from CLI input. Required by Config.
    pub fn parse_cli(&mut self, input: Option<&[u8]>) -> Result<(), ParseCliError> {
        let value = input.unwrap_or(b"");

        // Empty value resets the set
        if value.is_empty() {
            self.map.clear();
            self.mask = Mask::default();
            return Ok(());
        }

        self.parse(value).map_err(|err| match err {
            ParseError::MissingAssignment | ParseError::InvalidMod => ParseCliError::InvalidValue,
        })
    }

    /// Parse a modifier remap and add it to the set.
    pub fn parse(&mut self, input: &[u8]) -> Result<(), ParseError> {
        // Find the assignment point ('=')
        let eql_idx = input
            .iter()
            .position(|&b| b == b'=')
            .ok_or(ParseError::MissingAssignment)?;

        // The to side defaults to "left" if no explicit side is given.
        // This is because this is the default unsided value provided by
        // the apprts in the current Mods layout.
        let to: Mods = {
            let raw = parse_mod(&input[eql_idx + 1..])?;
            init_mods(raw.0, raw.1.unwrap_or(ModSide::Left))
        };

        // The from side, if sided, is easy and we put it directly into
        // the map.
        let from_raw = parse_mod(&input[..eql_idx])?;
        if let Some(from_side) = from_raw.1 {
            let from: Mods = init_mods(from_raw.0, from_side);
            self.map.insert(from, to);
            self.mask.update(from);
            return Ok(());
        }

        // We need to do some combinatorial explosion here for unsided
        // from in order to assign all possible sides.
        let from_left = init_mods(from_raw.0, ModSide::Left);
        let from_right = init_mods(from_raw.0, ModSide::Right);
        self.map.insert(from_left, to);
        self.map.insert(from_right, to);

        self.mask.update(from_left);
        self.mask.update(from_right);
        Ok(())
    }

    /// Must be called prior to any remappings so that the mapping
    /// is sorted properly. Otherwise, you will get invalid results.
    pub fn finalize(&mut self) {
        // Mods with any right sides prioritize.
        //
        // The Zig comparator (`lessThan(a, _) = a has any right side`) is not
        // a strict weak ordering; we use the equivalent well-formed stable
        // ordering: entries with a right side first, others after, each group
        // keeping insertion order.
        let side_mask = Mods::SIDE_MASK.int();
        self.map.sort_by(|a, _, b, _| {
            let a_right = a.int() & side_mask != 0;
            let b_right = b.int() & side_mask != 0;
            b_right.cmp(&a_right)
        });
    }

    /// Deep copy of the struct. Required by Config.
    pub fn clone_set(&self) -> RemapSet {
        RemapSet {
            map: self.map.clone(),
            mask: self.mask,
        }
    }

    /// Compare if two RemapSets are equal. Required by Config.
    pub fn equal(&self, other: &RemapSet) -> bool {
        if self.map.len() != other.map.len() {
            return false;
        }

        for (key, value) in self.map.iter() {
            let Some(other_value) = other.map.get(key) else {
                return false;
            };
            if !value.equal(*other_value) {
                return false;
            }
        }

        true
    }

    /// Used by Formatter. Required by Config.
    pub fn format_entry(
        &self,
        formatter: &mut EntryFormatter<'_>,
    ) -> Result<(), formatter::Error> {
        if self.map.is_empty() {
            formatter.format_entry(&())?;
            return Ok(());
        }

        for (from, to) in self.map.iter() {
            let mut writer: Vec<u8> = Vec::with_capacity(64);
            format_mod(&mut writer, *from).map_err(|_| formatter::Error::WriteFailed)?;
            writer.push(b'=');
            format_mod(&mut writer, *to).map_err(|_| formatter::Error::WriteFailed)?;
            formatter.format_entry(&*String::from_utf8_lossy(&writer))?;
        }

        Ok(())
    }

    /// Returns true if the given mods need remapping.
    #[inline]
    pub fn is_remapped(&self, mods: Mods) -> bool {
        self.mask.matches(mods)
    }

    /// Apply a remap to the given mods.
    pub fn apply(&self, mods: Mods) -> Mods {
        if !self.is_remapped(mods) {
            return mods;
        }

        let mods_binding: u8 = (mods.int() & 0xF) as u8;
        let mods_sides: u8 = mods.sides.int();

        for (from, to) in self.map.iter() {
            let from_binding: u8 = (from.int() & 0xF) as u8;
            if mods_binding & from_binding != from_binding {
                continue;
            }
            let from_sides: u8 = from.sides.int();
            if (mods_sides ^ from_sides) & from_binding != 0 {
                continue;
            }

            let mut mods_int = mods.int();
            mods_int &= !from.int();
            mods_int |= to.int();
            return Mods::from_int(mods_int);
        }

        unreachable!()
    }
}

impl PartialEq for RemapSet {
    fn eq(&self, other: &RemapSet) -> bool {
        self.equal(other)
    }
}

fn format_mod(writer: &mut Vec<u8>, mods: Mods) -> std::io::Result<()> {
    // Check which mod is set and format it with optional side prefix
    let fields: [(&str, bool, ModSide); 4] = [
        ("shift", mods.shift, mods.sides.shift),
        ("ctrl", mods.ctrl, mods.sides.ctrl),
        ("alt", mods.alt, mods.sides.alt),
        ("super", mods.super_, mods.sides.super_),
    ];
    for (name, set, side) in fields {
        if set {
            if side == ModSide::Right {
                writer.write_all(b"right_")?;
            } else {
                // Only write left_ if we need to distinguish
                // For now, always write left_ if it's a sided mapping
                writer.write_all(b"left_")?;
            }
            writer.write_all(name.as_bytes())?;
            return Ok(());
        }
    }

    Ok(())
}

/// Parses a single mode in a single remapping string. E.g.
/// `ctrl` or `left_shift`.
fn parse_mod(input: &[u8]) -> Result<(Mod, Option<ModSide>), ParseError> {
    let (side_str, mod_str): (&[u8], &[u8]) = match input.iter().position(|&b| b == b'_') {
        Some(idx) => (&input[..idx], &input[idx + 1..]),
        None => (b"", input),
    };

    let m: Mod = match Mod::parse(mod_str) {
        Some(m) => m,
        None => 'm: {
            for pair in ALIAS {
                if mod_str == pair.0.as_bytes() {
                    break 'm pair.1;
                }
            }

            return Err(ParseError::InvalidMod);
        }
    };

    Ok((
        m,
        if !side_str.is_empty() {
            Some(ModSide::parse(side_str).ok_or(ParseError::InvalidMod)?)
        } else {
            None
        },
    ))
}

fn init_mods(m: Mod, side: ModSide) -> Mods {
    let mut mods = Mods::EMPTY;
    match m {
        Mod::Shift => {
            mods.shift = true;
            mods.sides.shift = side;
        }
        Mod::Ctrl => {
            mods.ctrl = true;
            mods.sides.ctrl = side;
        }
        Mod::Alt => {
            mods.alt = true;
            mods.sides.alt = side;
        }
        Mod::Super => {
            mods.super_ = true;
            mods.sides.super_ = side;
        }
    }
    mods
}

/// Tracks which modifier keys and sides have remappings registered.
/// Used as a fast pre-check before doing expensive map lookups.
///
/// The mask uses separate tracking for left and right sides because
/// remappings can be side-specific (e.g., only remap left_ctrl).
///
/// Note: `left_sides` uses inverted logic where 1 means "left is remapped"
/// even though `Mod.Side.left = 0`. This allows efficient bitwise matching
/// since we can AND directly with the side bits.
///
/// (Zig: `RemapSet.Mask`, a `packed struct(u12)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mask {
    /// Which modifier keys (shift/ctrl/alt/super) have any remapping.
    pub keys: ModsKeys,
    /// Which modifiers have left-side remappings (inverted: 1 = left remapped).
    pub left_sides: ModsSide,
    /// Which modifiers have right-side remappings (1 = right remapped).
    pub right_sides: ModsSide,
}

impl Mask {
    /// Adds a modifier to the mask, marking it as having a remapping.
    pub fn update(&mut self, mods: Mods) {
        let keys_int: u8 = mods.keys().int();

        // OR the new keys into our existing keys mask.
        // Example: keys=0b0000, new ctrl → keys=0b0010
        self.keys = ModsKeys::from_int(self.keys.int() | keys_int);

        // Both Keys and Side are u4 with matching bit positions.
        // This lets us use keys_int to select which side bits to update.
        let sides: u8 = mods.sides.int();
        let left_int: u8 = self.left_sides.int();
        let right_int: u8 = self.right_sides.int();

        // Update left_sides: set bit if this key is active AND side is left.
        // Since Side.left=0, we invert sides (~sides) so left becomes 1.
        // keys_int masks to only affect the modifier being added.
        // Example: left_ctrl → keys_int=0b0010, ~sides=0b1111 (left=0 inverted)
        //          result: left_int | (0b0010 & 0b1111) = left_int | 0b0010
        self.left_sides = ModsSide::from_int((left_int | (keys_int & !sides)) & 0xF);

        // Update right_sides: set bit if this key is active AND side is right.
        // Since Side.right=1, we use sides directly.
        // Example: right_ctrl → keys_int=0b0010, sides=0b0010 (right=1)
        //          result: right_int | (0b0010 & 0b0010) = right_int | 0b0010
        self.right_sides = ModsSide::from_int(right_int | (keys_int & sides));
    }

    /// Returns true if the given mods match any remapping in this mask.
    /// This is a fast check to avoid expensive map lookups when no
    /// remapping could possibly apply.
    ///
    /// Checks both that the modifier key is remapped AND that the
    /// specific side (left/right) being pressed has a remapping.
    ///
    /// (Zig: `match`, a reserved word in Rust.)
    pub fn matches(&self, mods: Mods) -> bool {
        // Find which pressed keys have remappings registered.
        // Example: pressed={ctrl,alt}, mask={ctrl} → active=0b0010 (just ctrl)
        let active = mods.keys().int() & self.keys.int();
        if active == 0 {
            return false;
        }

        // Check if the pressed side matches a remapped side.
        // For left (sides bit = 0): check against left_int (where 1 = left remapped)
        //   ~sides inverts so left becomes 1, then AND with left_int
        // For right (sides bit = 1): check against right_int directly
        //
        // Example: pressing left_ctrl (sides.ctrl=0, left_int.ctrl=1)
        //   ~sides = 0b1111, left_int = 0b0010
        //   (~sides & left_int) = 0b0010 ✓ matches
        //
        // Example: pressing right_ctrl but only left_ctrl is remapped
        //   sides = 0b0010, left_int = 0b0010, right_int = 0b0000
        //   (~0b0010 & 0b0010) | (0b0010 & 0b0000) = 0b0000 ✗ no match
        let sides: u8 = mods.sides.int();
        let left_int: u8 = self.left_sides.int();
        let right_int: u8 = self.right_sides.int();
        let side_match = ((!sides & left_int) | (sides & right_int)) & 0xF;

        // Final check: is any active (pressed + remapped) key also side-matched?
        (active & side_match) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::formatter::entry_formatter;

    fn m(f: impl FnOnce(&mut Mods)) -> Mods {
        let mut v = Mods::EMPTY;
        f(&mut v);
        v
    }

    // For our own understanding
    #[test]
    fn mods_layout() {
        assert_eq!(Mods::EMPTY.int(), 0b0);
        assert_eq!(m(|v| v.shift = true).int(), 0b0000_0001);
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn translation_macos_option_as_alt() {
        // Unset
        {
            let mods = Mods::EMPTY;
            let result = mods.translation(OptionAsAlt::True);
            assert_eq!(result, mods);
        }

        // Set
        {
            let mods = m(|v| v.alt = true);
            let result = mods.translation(OptionAsAlt::True);
            assert_eq!(Mods::EMPTY, result);
        }

        // Set but disabled
        {
            let mods = m(|v| v.alt = true);
            let result = mods.translation(OptionAsAlt::False);
            assert_eq!(result, mods);
        }

        // Set wrong side
        {
            let mods = m(|v| {
                v.alt = true;
                v.sides.alt = ModSide::Right;
            });
            let result = mods.translation(OptionAsAlt::Left);
            assert_eq!(result, mods);
        }
        {
            let mods = m(|v| {
                v.alt = true;
                v.sides.alt = ModSide::Left;
            });
            let result = mods.translation(OptionAsAlt::Right);
            assert_eq!(result, mods);
        }

        // Set with other mods
        {
            let mods = m(|v| {
                v.alt = true;
                v.shift = true;
            });
            let result = mods.translation(OptionAsAlt::True);
            assert_eq!(m(|v| v.shift = true), result);
        }
    }

    fn sided(mod_: Mod, side: ModSide) -> Mods {
        init_mods(mod_, side)
    }

    #[test]
    fn remap_set_unsided_remap_creates_both_left_and_right_mappings() {
        let mut set = RemapSet::empty();
        set.parse(b"ctrl=super").unwrap();
        set.finalize();
        assert_eq!(
            sided(Mod::Super, ModSide::Left),
            set.apply(sided(Mod::Ctrl, ModSide::Left)),
        );
        assert_eq!(
            sided(Mod::Super, ModSide::Left),
            set.apply(sided(Mod::Ctrl, ModSide::Right)),
        );
    }

    #[test]
    fn remap_set_sided_from_only_maps_that_side() {
        let mut set = RemapSet::empty();

        set.parse(b"left_alt=ctrl").unwrap();
        set.finalize();

        let left_alt = sided(Mod::Alt, ModSide::Left);
        let left_ctrl = sided(Mod::Ctrl, ModSide::Left);
        assert_eq!(left_ctrl, set.apply(left_alt));

        let right_alt = sided(Mod::Alt, ModSide::Right);
        assert_eq!(right_alt, set.apply(right_alt));
    }

    #[test]
    fn remap_set_sided_to() {
        let mut set = RemapSet::empty();

        set.parse(b"ctrl=right_super").unwrap();
        set.finalize();

        let left_ctrl = sided(Mod::Ctrl, ModSide::Left);
        let right_super = sided(Mod::Super, ModSide::Right);
        assert_eq!(right_super, set.apply(left_ctrl));
    }

    #[test]
    fn remap_set_both_sides_specified() {
        let mut set = RemapSet::empty();

        set.parse(b"left_shift=right_ctrl").unwrap();
        set.finalize();

        let left_shift = sided(Mod::Shift, ModSide::Left);
        let right_ctrl = sided(Mod::Ctrl, ModSide::Right);
        assert_eq!(right_ctrl, set.apply(left_shift));
    }

    #[test]
    fn remap_set_multiple_parses_accumulate() {
        let mut set = RemapSet::empty();

        set.parse(b"left_ctrl=super").unwrap();
        set.parse(b"left_alt=ctrl").unwrap();
        set.finalize();

        let left_ctrl = sided(Mod::Ctrl, ModSide::Left);
        let left_super = sided(Mod::Super, ModSide::Left);
        assert_eq!(left_super, set.apply(left_ctrl));

        let left_alt = sided(Mod::Alt, ModSide::Left);
        let left_ctrl_result = sided(Mod::Ctrl, ModSide::Left);
        assert_eq!(left_ctrl_result, set.apply(left_alt));
    }

    #[test]
    fn remap_set_error_on_missing_assignment() {
        let mut set = RemapSet::empty();

        assert_eq!(Err(ParseError::MissingAssignment), set.parse(b"ctrl"));
        assert_eq!(Err(ParseError::MissingAssignment), set.parse(b""));
    }

    #[test]
    fn remap_set_error_on_invalid_modifier() {
        let mut set = RemapSet::empty();

        assert_eq!(Err(ParseError::InvalidMod), set.parse(b"invalid=ctrl"));
        assert_eq!(Err(ParseError::InvalidMod), set.parse(b"ctrl=invalid"));
        assert_eq!(Err(ParseError::InvalidMod), set.parse(b"middle_ctrl=super"));
    }

    #[test]
    fn remap_set_is_remapped_checks_mask() {
        let mut set = RemapSet::empty();

        set.parse(b"ctrl=super").unwrap();
        set.finalize();

        assert!(set.is_remapped(m(|v| v.ctrl = true)));
        assert!(!set.is_remapped(m(|v| v.alt = true)));
        assert!(!set.is_remapped(m(|v| v.shift = true)));
    }

    #[test]
    fn remap_set_clone_creates_independent_copy() {
        let mut set = RemapSet::empty();

        set.parse(b"ctrl=super").unwrap();
        set.finalize();

        let cloned = set.clone_set();

        assert!(set.equal(&cloned));
        assert!(cloned.is_remapped(m(|v| v.ctrl = true)));
    }

    #[test]
    fn remap_set_equal_compares_correctly() {
        let mut set1 = RemapSet::empty();
        let mut set2 = RemapSet::empty();

        assert!(set1.equal(&set2));

        set1.parse(b"ctrl=super").unwrap();
        assert!(!set1.equal(&set2));

        set2.parse(b"ctrl=super").unwrap();
        assert!(set1.equal(&set2));

        set1.parse(b"alt=shift").unwrap();
        assert!(!set1.equal(&set2));
    }

    #[test]
    fn remap_set_parse_cli_basic() {
        let mut set = RemapSet::empty();

        set.parse_cli(Some(b"ctrl=super")).unwrap();
        assert_eq!(2usize, set.map.len());
    }

    #[test]
    fn remap_set_parse_cli_empty_clears() {
        let mut set = RemapSet::empty();

        set.parse_cli(Some(b"ctrl=super")).unwrap();
        assert_eq!(2usize, set.map.len());

        set.parse_cli(Some(b"")).unwrap();
        assert_eq!(0usize, set.map.len());
    }

    #[test]
    fn remap_set_parse_cli_invalid() {
        let mut set = RemapSet::empty();

        assert_eq!(
            Err(ParseCliError::InvalidValue),
            set.parse_cli(Some(b"foo=bar"))
        );
        assert_eq!(
            Err(ParseCliError::InvalidValue),
            set.parse_cli(Some(b"ctrl"))
        );
    }

    #[test]
    fn remap_set_parse_aliased_modifiers() {
        let mut set = RemapSet::empty();

        set.parse(b"cmd=ctrl").unwrap();
        set.finalize();

        let left_super = sided(Mod::Super, ModSide::Left);
        let left_ctrl = sided(Mod::Ctrl, ModSide::Left);
        assert_eq!(left_ctrl, set.apply(left_super));
    }

    #[test]
    fn remap_set_parse_aliased_modifiers_command() {
        let mut set = RemapSet::empty();

        set.parse(b"command=alt").unwrap();
        set.finalize();

        let left_super = sided(Mod::Super, ModSide::Left);
        let left_alt = sided(Mod::Alt, ModSide::Left);
        assert_eq!(left_alt, set.apply(left_super));
    }

    #[test]
    fn remap_set_parse_aliased_modifiers_opt_and_option() {
        let mut set = RemapSet::empty();

        set.parse(b"opt=super").unwrap();
        set.finalize();

        let left_alt = sided(Mod::Alt, ModSide::Left);
        let left_super = sided(Mod::Super, ModSide::Left);
        assert_eq!(left_super, set.apply(left_alt));

        set = RemapSet::empty();

        set.parse(b"option=shift").unwrap();
        set.finalize();

        let left_shift = sided(Mod::Shift, ModSide::Left);
        assert_eq!(left_shift, set.apply(left_alt));
    }

    #[test]
    fn remap_set_parse_aliased_modifiers_control() {
        let mut set = RemapSet::empty();

        set.parse(b"control=super").unwrap();
        set.finalize();

        let left_ctrl = sided(Mod::Ctrl, ModSide::Left);
        let left_super = sided(Mod::Super, ModSide::Left);
        assert_eq!(left_super, set.apply(left_ctrl));
    }

    #[test]
    fn remap_set_parse_aliased_modifiers_on_target_side() {
        let mut set = RemapSet::empty();

        set.parse(b"alt=cmd").unwrap();
        set.finalize();

        let left_alt = sided(Mod::Alt, ModSide::Left);
        let left_super = sided(Mod::Super, ModSide::Left);
        assert_eq!(left_super, set.apply(left_alt));
    }

    #[test]
    fn remap_set_format_entry_empty() {
        let mut buf = String::new();

        let set = RemapSet::empty();
        set.format_entry(&mut entry_formatter("key-remap", &mut buf))
            .unwrap();
        assert_eq!("key-remap = \n", buf);
    }

    #[test]
    fn remap_set_format_entry_single_sided() {
        let mut buf = String::new();

        let mut set = RemapSet::empty();

        set.parse(b"left_ctrl=super").unwrap();
        set.finalize();

        set.format_entry(&mut entry_formatter("key-remap", &mut buf))
            .unwrap();
        assert_eq!("key-remap = left_ctrl=left_super\n", buf);
    }

    #[test]
    fn remap_set_format_entry_unsided_creates_two_entries() {
        let mut buf = String::new();

        let mut set = RemapSet::empty();

        set.parse(b"ctrl=super").unwrap();
        set.finalize();

        set.format_entry(&mut entry_formatter("key-remap", &mut buf))
            .unwrap();
        // Unsided creates both left and right mappings
        let written = buf;
        assert!(written.contains("left_ctrl=left_super"));
        assert!(written.contains("right_ctrl=left_super"));
    }

    #[test]
    fn remap_set_format_entry_right_sided() {
        let mut buf = String::new();

        let mut set = RemapSet::empty();

        set.parse(b"left_alt=right_ctrl").unwrap();
        set.finalize();

        set.format_entry(&mut entry_formatter("key-remap", &mut buf))
            .unwrap();
        assert_eq!("key-remap = left_alt=right_ctrl\n", buf);
    }

    // Port-specific: the bit layout round-trips.
    #[test]
    fn mods_int_round_trip() {
        for v in 0u16..(1 << 10) {
            assert_eq!(Mods::from_int(v).int(), v);
        }
        assert_eq!(Mods::SIDE_MASK.int(), 0b11_1100_0000);
    }
}
