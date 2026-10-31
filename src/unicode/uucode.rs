//! Rust stand-in for the external `uucode` Zig package
//! (jacobsandlund/uucode @ ea62149739404a73c202b48a33bf6dd2af4bd9b0, the
//! revision pinned in `build.zig.zon`), configured by
//! `src/build/uucode_config.zig`.
//!
//! Only the parts Ghostty uses are provided: the per-codepoint fields Ghostty
//! reads (`width`, `wcwidth_zero_in_grapheme`, `grapheme_break(_no_control)`,
//! `is_emoji_vs_base`, `is_symbol`, `is_emoji_presentation`,
//! `case_folding_full`), the grapheme break algorithm (`grapheme`), and the
//! ASCII helpers (`ascii`).
//!
//! The field data is generated from the Unicode 18.0.0 Character Database by
//! `tools/gen_unicode_tables.py` into `uucode_tables.rs` (a port of uucode's
//! table generation plus Ghostty's `uucode_config.zig` components).
//!
//! Zig `uucode.get(.field, cp)` maps to `uucode::field(cp)` here.

use super::lut::Tables;
use types::GraphemeBreak;
use types::GraphemeBreakNoControl;

/// uucode `config` constants.
pub mod config {
    pub const MAX_CODE_POINT: u32 = 0x10FFFF;
    pub const NUM_CODE_POINTS: u32 = MAX_CODE_POINT + 1;
    pub const ZERO_WIDTH_NON_JOINER: u32 = 0x200C;
    pub const ZERO_WIDTH_JOINER: u32 = 0x200D;
}

/// The Unicode version of the generated data.
pub const UNICODE_VERSION: &str = "18.0.0";

/// uucode `types` used by Ghostty.
pub mod types {
    /// uucode's derived grapheme break property (UAX #29 plus the emoji and
    /// Indic conjunct break properties needed to implement the rules).
    #[repr(u8)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub enum GraphemeBreak {
        #[default]
        Other,
        Control,
        Prepend,
        Cr,
        Lf,
        RegionalIndicator,
        SpacingMark,
        L,
        V,
        T,
        Lv,
        Lvt,
        Zwj,
        Zwnj,
        ExtendedPictographic,
        EmojiModifierBase,
        EmojiModifier,
        // extend, ==
        //   zwnj +
        //   indic_conjunct_break_extend +
        //   indic_conjunct_break_linker_extend
        IndicConjunctBreakExtend,
        IndicConjunctBreakLinkerExtend,
        // InCB=Linker with Grapheme_Cluster_Break=Other (U+1CF5, U+1CF6, U+11A3A
        // as of Unicode 18), so unlike `indic_conjunct_break_linker_extend` it is not
        // part of `extend`.
        IndicConjunctBreakLinkerOther,
        IndicConjunctBreakConsonant,
    }

    impl GraphemeBreak {
        /// All values in declaration (backing integer) order.
        pub const ALL: [GraphemeBreak; 21] = [
            Self::Other,
            Self::Control,
            Self::Prepend,
            Self::Cr,
            Self::Lf,
            Self::RegionalIndicator,
            Self::SpacingMark,
            Self::L,
            Self::V,
            Self::T,
            Self::Lv,
            Self::Lvt,
            Self::Zwj,
            Self::Zwnj,
            Self::ExtendedPictographic,
            Self::EmojiModifierBase,
            Self::EmojiModifier,
            Self::IndicConjunctBreakExtend,
            Self::IndicConjunctBreakLinkerExtend,
            Self::IndicConjunctBreakLinkerOther,
            Self::IndicConjunctBreakConsonant,
        ];

        /// The `grapheme_break_no_control` mapping: control, CR and LF
        /// become `other`.
        pub const fn no_control(self) -> GraphemeBreakNoControl {
            use GraphemeBreakNoControl as N;
            match self {
                Self::Other | Self::Control | Self::Cr | Self::Lf => N::Other,
                Self::Prepend => N::Prepend,
                Self::RegionalIndicator => N::RegionalIndicator,
                Self::SpacingMark => N::SpacingMark,
                Self::L => N::L,
                Self::V => N::V,
                Self::T => N::T,
                Self::Lv => N::Lv,
                Self::Lvt => N::Lvt,
                Self::Zwj => N::Zwj,
                Self::Zwnj => N::Zwnj,
                Self::ExtendedPictographic => N::ExtendedPictographic,
                Self::EmojiModifierBase => N::EmojiModifierBase,
                Self::EmojiModifier => N::EmojiModifier,
                Self::IndicConjunctBreakExtend => N::IndicConjunctBreakExtend,
                Self::IndicConjunctBreakLinkerExtend => N::IndicConjunctBreakLinkerExtend,
                Self::IndicConjunctBreakLinkerOther => N::IndicConjunctBreakLinkerOther,
                Self::IndicConjunctBreakConsonant => N::IndicConjunctBreakConsonant,
            }
        }
    }

    /// [`GraphemeBreak`] without `control`, `cr` and `lf`, which are assumed
    /// to have been handled prior or stripped from the input.
    #[repr(u8)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub enum GraphemeBreakNoControl {
        #[default]
        Other,
        Prepend,
        RegionalIndicator,
        SpacingMark,
        L,
        V,
        T,
        Lv,
        Lvt,
        Zwj,
        Zwnj,
        ExtendedPictographic,
        EmojiModifierBase,
        EmojiModifier,
        // extend, ==
        //   zwnj +
        //   indic_conjunct_break_extend +
        //   indic_conjunct_break_linker_extend
        IndicConjunctBreakExtend,
        IndicConjunctBreakLinkerExtend,
        // InCB=Linker with Grapheme_Cluster_Break=Other (U+1CF5, U+1CF6, U+11A3A
        // as of Unicode 18), so unlike `indic_conjunct_break_linker_extend` it is not
        // part of `extend`.
        IndicConjunctBreakLinkerOther,
        IndicConjunctBreakConsonant,
    }

    impl GraphemeBreakNoControl {
        /// All values in declaration (backing integer) order.
        pub const ALL: [GraphemeBreakNoControl; 18] = [
            Self::Other,
            Self::Prepend,
            Self::RegionalIndicator,
            Self::SpacingMark,
            Self::L,
            Self::V,
            Self::T,
            Self::Lv,
            Self::Lvt,
            Self::Zwj,
            Self::Zwnj,
            Self::ExtendedPictographic,
            Self::EmojiModifierBase,
            Self::EmojiModifier,
            Self::IndicConjunctBreakExtend,
            Self::IndicConjunctBreakLinkerExtend,
            Self::IndicConjunctBreakLinkerOther,
            Self::IndicConjunctBreakConsonant,
        ];

        /// The Zig tag name.
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Other => "other",
                Self::Prepend => "prepend",
                Self::RegionalIndicator => "regional_indicator",
                Self::SpacingMark => "spacing_mark",
                Self::L => "l",
                Self::V => "v",
                Self::T => "t",
                Self::Lv => "lv",
                Self::Lvt => "lvt",
                Self::Zwj => "zwj",
                Self::Zwnj => "zwnj",
                Self::ExtendedPictographic => "extended_pictographic",
                Self::EmojiModifierBase => "emoji_modifier_base",
                Self::EmojiModifier => "emoji_modifier",
                Self::IndicConjunctBreakExtend => "indic_conjunct_break_extend",
                Self::IndicConjunctBreakLinkerExtend => "indic_conjunct_break_linker_extend",
                Self::IndicConjunctBreakLinkerOther => "indic_conjunct_break_linker_other",
                Self::IndicConjunctBreakConsonant => "indic_conjunct_break_consonant",
            }
        }

        /// The Rust variant name (used when emitting generated tables).
        pub const fn variant_name(self) -> &'static str {
            match self {
                Self::Other => "Other",
                Self::Prepend => "Prepend",
                Self::RegionalIndicator => "RegionalIndicator",
                Self::SpacingMark => "SpacingMark",
                Self::L => "L",
                Self::V => "V",
                Self::T => "T",
                Self::Lv => "Lv",
                Self::Lvt => "Lvt",
                Self::Zwj => "Zwj",
                Self::Zwnj => "Zwnj",
                Self::ExtendedPictographic => "ExtendedPictographic",
                Self::EmojiModifierBase => "EmojiModifierBase",
                Self::EmojiModifier => "EmojiModifier",
                Self::IndicConjunctBreakExtend => "IndicConjunctBreakExtend",
                Self::IndicConjunctBreakLinkerExtend => "IndicConjunctBreakLinkerExtend",
                Self::IndicConjunctBreakLinkerOther => "IndicConjunctBreakLinkerOther",
                Self::IndicConjunctBreakConsonant => "IndicConjunctBreakConsonant",
            }
        }
    }
}

/// uucode `ascii`.
pub mod ascii {
    /// Returns whether the code point is alphanumeric: A-Z, a-z, or 0-9.
    pub fn is_alphanumeric(c: u32) -> bool {
        matches!(c, 0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A)
    }

    /// Returns whether the code point is alphabetic: A-Z or a-z.
    pub fn is_alphabetic(c: u32) -> bool {
        matches!(c, 0x41..=0x5A | 0x61..=0x7A)
    }

    /// Returns whether the code point is a control character.
    pub fn is_control(c: u32) -> bool {
        c <= 0x1F || c == 0x7F
    }

    /// Returns whether the code point is a digit.
    pub fn is_digit(c: u32) -> bool {
        matches!(c, 0x30..=0x39)
    }

    /// Returns whether the code point is a lowercase letter.
    pub fn is_lower(c: u32) -> bool {
        matches!(c, 0x61..=0x7A)
    }

    /// Returns whether the code point is printable and has some graphical
    /// representation, including the space code point.
    pub fn is_print(c: u32) -> bool {
        is_ascii(c) && !is_control(c)
    }

    /// Returns whether this code point is ASCII whitespace.
    pub fn is_whitespace(c: u32) -> bool {
        matches!(c, 0x20 | 0x09..=0x0D)
    }

    /// Returns whether the code point is an uppercase letter.
    pub fn is_upper(c: u32) -> bool {
        matches!(c, 0x41..=0x5A)
    }

    /// Returns whether the code point is a hexadecimal digit: A-F, a-f, or 0-9.
    pub fn is_hex(c: u32) -> bool {
        matches!(c, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66)
    }

    /// Returns whether the code point is a 7-bit ASCII character.
    pub fn is_ascii(c: u32) -> bool {
        c < 128
    }

    /// Uppercases the code point and returns it as-is if already uppercase or
    /// not a letter.
    pub fn to_upper(c: u32) -> u32 {
        let mask = (is_lower(c) as u32) << 5;
        c ^ mask
    }

    /// Lowercases the code point and returns it as-is if already lowercase or
    /// not a letter.
    pub fn to_lower(c: u32) -> u32 {
        let mask = (is_upper(c) as u32) << 5;
        c | mask
    }
}

/// uucode `grapheme`: the (tailored) UAX #29 grapheme cluster break
/// algorithm.
pub mod grapheme {
    use super::types::{GraphemeBreak, GraphemeBreakNoControl};

    /// GB9c can overlap GB11: after `ExtPict Linker`, `Extend* ZWJ ExtPict` is
    /// still an emoji ZWJ sequence while `Extend* Consonant` is still a
    /// conjunct. So "after a linker" is tracked alongside `base` rather than
    /// as a state of its own.
    ///
    /// The Zig type is a `packed struct(u3)` whose bit pattern
    /// (`base * 2 + after_linker`) is the precomputed table index; see
    /// [`BreakState::table_index`]. The combination `regional_indicator` with
    /// `after_linker` can never occur, so tables leave it out.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct BreakState {
        pub after_linker: bool,
        pub base: Base,
    }

    #[repr(u8)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub enum Base {
        #[default]
        Default,
        ExtendedPictographic,
        RegionalIndicator,
    }

    impl BreakState {
        /// Deprecated in uucode: initialize with `BreakState::default()`.
        pub const DEFAULT: BreakState = BreakState {
            after_linker: false,
            base: Base::Default,
        };

        pub const TABLE_LEN: usize = 3 * 2 - 1;

        #[inline(always)]
        pub const fn table_index(self) -> usize {
            (self.after_linker as usize) | ((self.base as usize) << 1)
        }

        #[inline(always)]
        pub const fn from_table_index(i: usize) -> BreakState {
            BreakState {
                after_linker: i & 1 != 0,
                base: match (i >> 1) & 0b11 {
                    0 => Base::Default,
                    1 => Base::ExtendedPictographic,
                    2 => Base::RegionalIndicator,
                    _ => panic!("invalid BreakState table index"),
                },
            }
        }
    }

    pub const fn compute_grapheme_break(
        gb1: GraphemeBreak,
        gb2: GraphemeBreak,
        state: &mut BreakState,
    ) -> bool {
        use GraphemeBreak as G;

        // Whether the next flag is set depends only on the input, not on the
        // rules below, which only read `after_linker` and only write `base`.
        let after_linker = is_indic_conjunct_break_linker(gb1)
            || (state.after_linker && is_indic_conjunct_break_extend(gb1));
        state.after_linker = after_linker && is_indic_conjunct_break_extend(gb2);

        // Set state back to default when `gb1` or `gb2` is not expected in sequence.
        match state.base {
            Base::RegionalIndicator => {
                if !matches!(gb1, G::RegionalIndicator) || !matches!(gb2, G::RegionalIndicator) {
                    state.base = Base::Default;
                }
            }
            Base::ExtendedPictographic => {
                // Keep state if in possibly valid sequence
                if !matches!(
                    gb1,
                    G::IndicConjunctBreakExtend
                        | G::IndicConjunctBreakLinkerExtend
                        | G::Zwnj
                        | G::Zwj
                        | G::ExtendedPictographic
                        | G::EmojiModifierBase
                        | G::EmojiModifier
                ) {
                    state.base = Base::Default;
                }
                if !matches!(
                    gb2,
                    G::IndicConjunctBreakExtend
                        | G::IndicConjunctBreakLinkerExtend
                        | G::Zwnj
                        | G::Zwj
                        | G::ExtendedPictographic
                        | G::EmojiModifierBase
                        | G::EmojiModifier
                ) {
                    state.base = Base::Default;
                }
            }
            Base::Default => {}
        }

        // GB3: CR x LF
        if matches!(gb1, G::Cr) && matches!(gb2, G::Lf) {
            return false;
        }

        // GB4: Control
        if matches!(gb1, G::Control | G::Cr | G::Lf) {
            return true;
        }

        // GB5: Control
        if matches!(gb2, G::Control | G::Cr | G::Lf) {
            return true;
        }

        // GB6: L x (L | V | LV | VT)
        if matches!(gb1, G::L) && matches!(gb2, G::L | G::V | G::Lv | G::Lvt) {
            return false;
        }

        // GB7: (LV | V) x (V | T)
        if matches!(gb1, G::Lv | G::V) && matches!(gb2, G::V | G::T) {
            return false;
        }

        // GB8: (LVT | T) x T
        if matches!(gb1, G::Lvt | G::T) && matches!(gb2, G::T) {
            return false;
        }

        // Handle GB9 (Extend | ZWJ) later, since it can also match the start of
        // GB9c (Indic) and GB11 (Emoji ZWJ)

        // GB9a: SpacingMark
        if matches!(gb2, G::SpacingMark) {
            return false;
        }

        // GB9b: Prepend
        if matches!(gb1, G::Prepend) {
            return false;
        }

        // GB9c: InCB=Linker InCB=Extend* x InCB=Consonant
        if after_linker && matches!(gb2, G::IndicConjunctBreakConsonant) {
            state.base = Base::Default;
            return false;
        }

        // GB11: Emoji ZWJ sequence and Emoji modifier sequence
        if is_extended_pictographic(gb1) {
            // start of sequence:
            if is_extend(gb2) || matches!(gb2, G::Zwj) {
                state.base = Base::ExtendedPictographic;
                return false;
            }

            // The `emoji_modifier_sequence` case is described in the comment
            // for `is_extend` below, from UTS #51.
            if matches!(gb1, G::EmojiModifierBase) && matches!(gb2, G::EmojiModifier) {
                state.base = Base::ExtendedPictographic;
                return false;
            }

            // else, not an Emoji ZWJ sequence
        } else if matches!(state.base, Base::ExtendedPictographic) {
            // continue or end sequence:
            if (is_extend(gb1) || matches!(gb1, G::EmojiModifier))
                && (is_extend(gb2) || matches!(gb2, G::Zwj))
            {
                // continue extend* ZWJ sequence
                return false;
            } else if matches!(gb1, G::Zwj) && is_extended_pictographic(gb2) {
                // ZWJ -> end of sequence
                state.base = Base::Default;
                return false;
            } else {
                // Not a valid Emoji ZWJ sequence
                state.base = Base::Default;
            }
        }

        // GB12 and GB13: Regional Indicator
        if matches!(gb1, G::RegionalIndicator) && matches!(gb2, G::RegionalIndicator) {
            if matches!(state.base, Base::Default) {
                state.base = Base::RegionalIndicator;
                return false;
            } else {
                state.base = Base::Default;
                return true;
            }
        }

        // GB9: x (Extend | ZWJ)
        if is_extend(gb2) || matches!(gb2, G::Zwj) {
            return false;
        }

        // GB999: Otherwise, break everywhere
        true
    }

    const fn is_indic_conjunct_break_extend(gb: GraphemeBreak) -> bool {
        matches!(gb, GraphemeBreak::IndicConjunctBreakExtend | GraphemeBreak::Zwj)
    }

    const fn is_indic_conjunct_break_linker(gb: GraphemeBreak) -> bool {
        matches!(
            gb,
            GraphemeBreak::IndicConjunctBreakLinkerExtend | GraphemeBreak::IndicConjunctBreakLinkerOther
        )
    }

    // Despite `emoji_modifier` being `extend` according to
    // GraphemeBreakProperty.txt and UAX #29 (in addition to tests in
    // GraphemeBreakTest.txt), UTS #51 states: `emoji_modifier_sequence :=
    // emoji_modifier_base emoji_modifier` in ED-13 (emoji modifier sequence)
    // under 1.4.4 (Emoji Modifiers), and: "When used alone, the default
    // representation of these modifier characters is a color swatch... To
    // have an effect on an emoji, an emoji modifier must immediately follow
    // that base emoji character." in 2.4 (Diversity). Additionally it states
    // "Skin tone modifiers and hair components should be displayed even in
    // isolation" in ED-20 (basic emoji set) under 1.4.6 (Emoji Sets). See this
    // revision of UAX #29 when the grapheme cluster break properties were
    // simplified to remove `E_Base` and `E_Modifier`:
    // http://www.unicode.org/reports/tr29/tr29-32.html
    // Here we decide to diverge from the grapheme break spec, which is allowed
    // under "tailored" grapheme clusters.
    const fn is_extend(gb: GraphemeBreak) -> bool {
        matches!(
            gb,
            GraphemeBreak::Zwnj
                | GraphemeBreak::IndicConjunctBreakExtend
                | GraphemeBreak::IndicConjunctBreakLinkerExtend
        )
    }

    const fn is_extended_pictographic(gb: GraphemeBreak) -> bool {
        matches!(gb, GraphemeBreak::ExtendedPictographic | GraphemeBreak::EmojiModifierBase)
    }

    /// This is a copy of `compute_grapheme_break` but with the rules for
    /// `control`, `cr`, and `lf` ignored, since `grapheme_break_no_control`
    /// maps them to `other` as these are assumed to have been handled prior
    /// or stripped from the input.
    pub const fn compute_grapheme_break_no_control(
        gb1: GraphemeBreakNoControl,
        gb2: GraphemeBreakNoControl,
        state: &mut BreakState,
    ) -> bool {
        use GraphemeBreakNoControl as G;

        let after_linker = is_indic_conjunct_break_linker_no_control(gb1)
            || (state.after_linker && is_indic_conjunct_break_extend_no_control(gb1));
        state.after_linker = after_linker && is_indic_conjunct_break_extend_no_control(gb2);

        match state.base {
            Base::RegionalIndicator => {
                if !matches!(gb1, G::RegionalIndicator) || !matches!(gb2, G::RegionalIndicator) {
                    state.base = Base::Default;
                }
            }
            Base::ExtendedPictographic => {
                if !matches!(
                    gb1,
                    G::IndicConjunctBreakExtend
                        | G::IndicConjunctBreakLinkerExtend
                        | G::Zwnj
                        | G::Zwj
                        | G::ExtendedPictographic
                        | G::EmojiModifierBase
                        | G::EmojiModifier
                ) {
                    state.base = Base::Default;
                }
                if !matches!(
                    gb2,
                    G::IndicConjunctBreakExtend
                        | G::IndicConjunctBreakLinkerExtend
                        | G::Zwnj
                        | G::Zwj
                        | G::ExtendedPictographic
                        | G::EmojiModifierBase
                        | G::EmojiModifier
                ) {
                    state.base = Base::Default;
                }
            }
            Base::Default => {}
        }

        // GB6: L x (L | V | LV | VT)
        if matches!(gb1, G::L) && matches!(gb2, G::L | G::V | G::Lv | G::Lvt) {
            return false;
        }

        // GB7: (LV | V) x (V | T)
        if matches!(gb1, G::Lv | G::V) && matches!(gb2, G::V | G::T) {
            return false;
        }

        // GB8: (LVT | T) x T
        if matches!(gb1, G::Lvt | G::T) && matches!(gb2, G::T) {
            return false;
        }

        // GB9a: SpacingMark
        if matches!(gb2, G::SpacingMark) {
            return false;
        }

        // GB9b: Prepend
        if matches!(gb1, G::Prepend) {
            return false;
        }

        // GB9c: InCB=Linker InCB=Extend* x InCB=Consonant
        if after_linker && matches!(gb2, G::IndicConjunctBreakConsonant) {
            state.base = Base::Default;
            return false;
        }

        // GB11: Emoji ZWJ sequence and Emoji modifier sequence
        if is_extended_pictographic_no_control(gb1) {
            if is_extend_no_control(gb2) || matches!(gb2, G::Zwj) {
                state.base = Base::ExtendedPictographic;
                return false;
            }

            if matches!(gb1, G::EmojiModifierBase) && matches!(gb2, G::EmojiModifier) {
                state.base = Base::ExtendedPictographic;
                return false;
            }
        } else if matches!(state.base, Base::ExtendedPictographic) {
            if (is_extend_no_control(gb1) || matches!(gb1, G::EmojiModifier))
                && (is_extend_no_control(gb2) || matches!(gb2, G::Zwj))
            {
                return false;
            } else if matches!(gb1, G::Zwj) && is_extended_pictographic_no_control(gb2) {
                state.base = Base::Default;
                return false;
            } else {
                state.base = Base::Default;
            }
        }

        // GB12 and GB13: Regional Indicator
        if matches!(gb1, G::RegionalIndicator) && matches!(gb2, G::RegionalIndicator) {
            if matches!(state.base, Base::Default) {
                state.base = Base::RegionalIndicator;
                return false;
            } else {
                state.base = Base::Default;
                return true;
            }
        }

        // GB9: x (Extend | ZWJ)
        if is_extend_no_control(gb2) || matches!(gb2, G::Zwj) {
            return false;
        }

        // GB999: Otherwise, break everywhere
        true
    }

    const fn is_indic_conjunct_break_extend_no_control(gb: GraphemeBreakNoControl) -> bool {
        matches!(
            gb,
            GraphemeBreakNoControl::IndicConjunctBreakExtend | GraphemeBreakNoControl::Zwj
        )
    }

    const fn is_indic_conjunct_break_linker_no_control(gb: GraphemeBreakNoControl) -> bool {
        matches!(
            gb,
            GraphemeBreakNoControl::IndicConjunctBreakLinkerExtend
                | GraphemeBreakNoControl::IndicConjunctBreakLinkerOther
        )
    }

    // See the comment above `is_extend`.
    const fn is_extend_no_control(gb: GraphemeBreakNoControl) -> bool {
        matches!(
            gb,
            GraphemeBreakNoControl::Zwnj
                | GraphemeBreakNoControl::IndicConjunctBreakExtend
                | GraphemeBreakNoControl::IndicConjunctBreakLinkerExtend
        )
    }

    const fn is_extended_pictographic_no_control(gb: GraphemeBreakNoControl) -> bool {
        matches!(
            gb,
            GraphemeBreakNoControl::ExtendedPictographic | GraphemeBreakNoControl::EmojiModifierBase
        )
    }

    // Precomputed tables (uucode `buildGraphemeBreakTable`), indexed by
    // `state * n_gb^2 + gb1 * n_gb + gb2`. Each entry packs the result in
    // bit 0 and the new state's table index in bits 1..=3.

    const N_GB: usize = GraphemeBreak::ALL.len();
    const N_GB_NC: usize = GraphemeBreakNoControl::ALL.len();

    const fn pack(result: bool, state: BreakState) -> u8 {
        (result as u8) | ((state.table_index() as u8) << 1)
    }

    static GRAPHEME_BREAK_TABLE: [u8; BreakState::TABLE_LEN * N_GB * N_GB] = {
        let mut table = [0u8; BreakState::TABLE_LEN * N_GB * N_GB];
        let mut s = 0;
        while s < BreakState::TABLE_LEN {
            let mut i = 0;
            while i < N_GB {
                let mut j = 0;
                while j < N_GB {
                    let mut state = BreakState::from_table_index(s);
                    let result = compute_grapheme_break(
                        GraphemeBreak::ALL[i],
                        GraphemeBreak::ALL[j],
                        &mut state,
                    );
                    // The table must never hand back a state it has no row for.
                    assert!(state.table_index() < BreakState::TABLE_LEN);
                    table[s * N_GB * N_GB + i * N_GB + j] = pack(result, state);
                    j += 1;
                }
                i += 1;
            }
            s += 1;
        }
        table
    };

    static GRAPHEME_BREAK_NO_CONTROL_TABLE: [u8; BreakState::TABLE_LEN * N_GB_NC * N_GB_NC] = {
        let mut table = [0u8; BreakState::TABLE_LEN * N_GB_NC * N_GB_NC];
        let mut s = 0;
        while s < BreakState::TABLE_LEN {
            let mut i = 0;
            while i < N_GB_NC {
                let mut j = 0;
                while j < N_GB_NC {
                    let mut state = BreakState::from_table_index(s);
                    let result = compute_grapheme_break_no_control(
                        GraphemeBreakNoControl::ALL[i],
                        GraphemeBreakNoControl::ALL[j],
                        &mut state,
                    );
                    assert!(state.table_index() < BreakState::TABLE_LEN);
                    table[s * N_GB_NC * N_GB_NC + i * N_GB_NC + j] = pack(result, state);
                    j += 1;
                }
                i += 1;
            }
            s += 1;
        }
        table
    };

    pub fn precomputed_grapheme_break(
        gb1: GraphemeBreak,
        gb2: GraphemeBreak,
        state: &mut BreakState,
    ) -> bool {
        let v = GRAPHEME_BREAK_TABLE
            [state.table_index() * N_GB * N_GB + gb1 as usize * N_GB + gb2 as usize];
        *state = BreakState::from_table_index((v >> 1) as usize);
        v & 1 != 0
    }

    pub fn is_break(cp1: u32, cp2: u32, state: &mut BreakState) -> bool {
        let gb1 = super::grapheme_break(cp1);
        let gb2 = super::grapheme_break(cp2);
        precomputed_grapheme_break(gb1, gb2, state)
    }

    pub fn precomputed_grapheme_break_no_control(
        gb1: GraphemeBreakNoControl,
        gb2: GraphemeBreakNoControl,
        state: &mut BreakState,
    ) -> bool {
        let v = GRAPHEME_BREAK_NO_CONTROL_TABLE
            [state.table_index() * N_GB_NC * N_GB_NC + gb1 as usize * N_GB_NC + gb2 as usize];
        *state = BreakState::from_table_index((v >> 1) as usize);
        v & 1 != 0
    }

    pub fn is_break_no_control(cp1: u32, cp2: u32, state: &mut BreakState) -> bool {
        let gb1 = super::grapheme_break_no_control(cp1);
        let gb2 = super::grapheme_break_no_control(cp2);
        precomputed_grapheme_break_no_control(gb1, gb2, state)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn break_state_table_index_layout() {
            assert_eq!(0, BreakState::default().table_index());
            assert_eq!(
                1,
                BreakState { after_linker: true, ..Default::default() }.table_index()
            );
            assert_eq!(
                2,
                BreakState { base: Base::ExtendedPictographic, ..Default::default() }
                    .table_index()
            );
            assert_eq!(
                3,
                BreakState { base: Base::ExtendedPictographic, after_linker: true }
                    .table_index()
            );
            assert_eq!(
                4,
                BreakState { base: Base::RegionalIndicator, ..Default::default() }
                    .table_index()
            );
            assert_eq!(5, BreakState::TABLE_LEN);
            assert_eq!(
                5,
                BreakState { base: Base::RegionalIndicator, after_linker: true }.table_index()
            );
            for i in 0..BreakState::TABLE_LEN {
                assert_eq!(i, BreakState::from_table_index(i).table_index());
            }
        }

        fn computed(cp1: u32, cp2: u32, state: &mut BreakState) -> bool {
            compute_grapheme_break(super::super::grapheme_break(cp1), super::super::grapheme_break(cp2), state)
        }

        fn computed_no_control(cp1: u32, cp2: u32, state: &mut BreakState) -> bool {
            compute_grapheme_break_no_control(
                super::super::grapheme_break_no_control(cp1),
                super::super::grapheme_break_no_control(cp2),
                state,
            )
        }

        #[test]
        fn unicode_18_indic_linker_boundaries_and_overlapping_emoji_sequences() {
            let cases: &[(&[u32], &[bool])] = &[
                // GB9c no longer needs a leading consonant: Linker Extend* x Consonant
                (&[0x094D, 0x0915], &[false]),
                (&[0x0061, 0x094D, 0x0915], &[false, false]),
                (&[0x094D, 0x0300, 0x200D, 0x0915], &[false, false, false]),
                (&[0x094D, 0x094D, 0x0915], &[false, false]),
                // Extend without a linker doesn't join a consonant
                (&[0x0915, 0x0300, 0x0915], &[false, true]),
                // A consonant ends the linker sequence
                (&[0x094D, 0x0915, 0x0300, 0x0915], &[false, false, true]),
                // Only InCB=Extend continues the sequence: ZWNJ, SpacingMark
                // and an emoji modifier all end it
                (&[0x094D, 0x200C, 0x0300, 0x0915], &[false, false, true]),
                (&[0x094D, 0x0903, 0x0915], &[false, true]),
                (&[0x094D, 0x1F3FB, 0x0915], &[true, true]),
                // InCB=Linker with Grapheme_Cluster_Break=Other: breaks
                // before it, but still joins a following consonant
                (&[0x0061, 0x1CF5, 0x0300, 0x0915], &[true, false, false]),
                (&[0x0061, 0x1CF6, 0x0915], &[true, false]),
                (&[0x0061, 0x11A3A, 0x11A0B], &[true, false]),
                (&[0x11A3A, 0x0061], &[true]),
                // GB9c and GB11 overlap: a linker inside an emoji sequence
                // can still lead to either a ZWJ emoji or a consonant
                (&[0x1F600, 0x094D, 0x0300, 0x200D, 0x1F600], &[false, false, false, false]),
                (&[0x1F600, 0x094D, 0x0300, 0x0915], &[false, false, false]),
                (&[0x1F600, 0x094D, 0x200C, 0x200D, 0x1F600], &[false, false, false, false]),
                (&[0x1F600, 0x094D, 0x200C, 0x0915], &[false, false, true]),
                // An emoji modifier sequence can lead into GB9c
                (&[0x1F44D, 0x1F3FB, 0x094D, 0x0915], &[false, false, false]),
                // A Grapheme_Cluster_Break=Other linker ends the emoji sequence
                (&[0x1F600, 0x1CF5, 0x0915], &[true, false]),
            ];
            let checks: [fn(u32, u32, &mut BreakState) -> bool; 4] =
                [computed, is_break, computed_no_control, is_break_no_control];
            for check in checks {
                for (cps, breaks) in cases {
                    let mut state = BreakState::default();
                    for (i, &expected) in breaks.iter().enumerate() {
                        assert_eq!(expected, check(cps[i], cps[i + 1], &mut state));
                    }
                }
            }
        }
    }
}

/// One row of the generated uucode table: the fields Ghostty reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// Ghostty's `width` field (uucode_config.zig `WidthComponent`).
    pub width: u8,
    pub wcwidth_zero_in_grapheme: bool,
    pub grapheme_break: GraphemeBreak,
    pub is_emoji_vs_base: bool,
    /// Ghostty's `is_symbol` field (uucode_config.zig `IsSymbolComponent`).
    pub is_symbol: bool,
    pub is_emoji_presentation: bool,
}

mod generated {
    #![allow(clippy::all)]
    use super::types::GraphemeBreak;
    use super::Row;
    include!("uucode_tables.rs");
}

static TABLE: Tables<'static, Row> = Tables {
    stage1: &generated::STAGE1,
    stage2: &generated::STAGE2,
    stage3: &generated::STAGE3,
};

#[inline(always)]
fn row(cp: u32) -> Row {
    debug_assert!(cp <= config::MAX_CODE_POINT);
    TABLE.get(cp)
}

/// `uucode.get(.width, cp)` (Ghostty's clamped terminal width).
#[inline]
pub fn width(cp: u32) -> u8 {
    row(cp).width
}

/// `uucode.get(.wcwidth_zero_in_grapheme, cp)`.
#[inline]
pub fn wcwidth_zero_in_grapheme(cp: u32) -> bool {
    row(cp).wcwidth_zero_in_grapheme
}

/// `uucode.get(.grapheme_break, cp)`.
#[inline]
pub fn grapheme_break(cp: u32) -> GraphemeBreak {
    row(cp).grapheme_break
}

/// `uucode.get(.grapheme_break_no_control, cp)`.
#[inline]
pub fn grapheme_break_no_control(cp: u32) -> GraphemeBreakNoControl {
    row(cp).grapheme_break.no_control()
}

/// `uucode.get(.is_emoji_vs_base, cp)`: whether cp has an entry in
/// emoji-variation-sequences.txt.
#[inline]
pub fn is_emoji_vs_base(cp: u32) -> bool {
    row(cp).is_emoji_vs_base
}

/// `uucode.get(.is_symbol, cp)`.
#[inline]
pub fn is_symbol(cp: u32) -> bool {
    row(cp).is_symbol
}

/// `uucode.get(.is_emoji_presentation, cp)`.
#[inline]
pub fn is_emoji_presentation(cp: u32) -> bool {
    row(cp).is_emoji_presentation
}

/// The full case folding (CaseFolding.txt status C + F) of a codepoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaseFoldingFull {
    len: u8,
    cps: [u32; 3],
}

impl CaseFoldingFull {
    /// The mapped codepoints (`uucode.get(.case_folding_full, cp).with(&buf, cp)`).
    pub fn as_slice(&self) -> &[u32] {
        &self.cps[..self.len as usize]
    }
}

/// `uucode.get(.case_folding_full, cp)`.
pub fn case_folding_full(cp: u32) -> CaseFoldingFull {
    match generated::CASE_FOLDING_FULL.binary_search_by_key(&cp, |e| e.0) {
        Ok(i) => {
            let (_, len, cps) = generated::CASE_FOLDING_FULL[i];
            CaseFoldingFull { len, cps }
        }
        Err(_) => CaseFoldingFull {
            len: 1,
            cps: [cp, 0, 0],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::grapheme::*;
    use super::*;

    #[test]
    fn fields() {
        assert!(is_emoji_presentation(0x1F600));
        assert!(!is_emoji_presentation('a' as u32));
        assert!(!is_emoji_presentation(0x2764));
        assert!(is_emoji_vs_base(0x2764));
        assert!(is_symbol(0x2190)); // arrows
        assert!(is_symbol(0xE000)); // private use
        assert!(!is_symbol('a' as u32));
        assert_eq!(GraphemeBreak::Control, grapheme_break(0x07));
        assert_eq!(GraphemeBreak::Cr, grapheme_break(0x0D));
        assert_eq!(GraphemeBreakNoControl::Other, grapheme_break_no_control(0x0A));
        assert_eq!(GraphemeBreak::EmojiModifier, grapheme_break(0x1F3FB));
    }

    #[test]
    fn case_folding() {
        assert_eq!(&['a' as u32], case_folding_full('A' as u32).as_slice());
        assert_eq!(&['a' as u32], case_folding_full('a' as u32).as_slice());
        assert_eq!(&[0x73, 0x73], case_folding_full(0xDF).as_slice()); // ß
        assert_eq!(&[0x3C3], case_folding_full(0x3A3).as_slice()); // Σ
        assert_eq!(&[0x1F600], case_folding_full(0x1F600).as_slice());
    }

    #[test]
    fn ascii_helpers() {
        assert!(ascii::is_alphabetic('a' as u32));
        assert!(!ascii::is_alphabetic('1' as u32));
        assert_eq!('a' as u32, ascii::to_lower('A' as u32));
        assert_eq!('A' as u32, ascii::to_upper('a' as u32));
        assert_eq!('1' as u32, ascii::to_lower('1' as u32));
    }

    /// GraphemeBreakTest.txt conformance (uucode's `testGraphemeBreak`).
    fn check_grapheme_break_test(actual_is_break: fn(u32, u32, &mut BreakState) -> bool) {
        let content = include_str!("testdata/GraphemeBreakTest.txt");
        let mut success = true;
        for (line_num, line) in content.lines().enumerate() {
            let trimmed = trim(line);
            if trimmed.is_empty() {
                continue;
            }
            let parts: Vec<&str> = trimmed.split(' ').collect();
            assert_eq!("÷", parts[0]);
            let mut state = BreakState::default();
            let mut i = 1;
            while i + 2 < parts.len() {
                let cp1 = u32::from_str_radix(parts[i], 16).unwrap();
                let expected_str = parts[i + 1];
                let cp2 = u32::from_str_radix(parts[i + 2], 16).unwrap();
                let gb1 = grapheme_break(cp1);
                let gb2 = grapheme_break(cp2);
                assert!(expected_str == "÷" || expected_str == "×");
                let mut expected_is_break = expected_str == "÷";
                let actual = actual_is_break(cp1, cp2, &mut state);
                // GraphemeBreakTest.txt has tests for UAX #29 treating emoji
                // modifier as extend, always, but we diverge from that.
                if gb2 == GraphemeBreak::EmojiModifier && gb1 != GraphemeBreak::EmojiModifierBase {
                    assert!(!expected_is_break);
                    expected_is_break = true;
                }
                if actual != expected_is_break {
                    eprintln!(
                        "line={} cp1={:x}, cp2={:x}: gb1={:?}, gb2={:?}, state={:?}, expected={}, actual={}",
                        line_num + 1, cp1, cp2, gb1, gb2, state, expected_is_break, actual
                    );
                    success = false;
                }
                i += 2;
            }
            assert_eq!("÷", parts[parts.len() - 1]);
        }
        assert!(success);
    }

    /// GraphemeBreakTest.txt conformance for the no-control variant
    /// (uucode's `testGraphemeBreakNoControl`): lines are cut off at the
    /// first control/CR/LF.
    fn check_grapheme_break_test_no_control(
        actual_is_break: fn(u32, u32, &mut BreakState) -> bool,
    ) {
        let content = include_str!("testdata/GraphemeBreakTest.txt");
        let is_control = |gb: GraphemeBreak| {
            matches!(gb, GraphemeBreak::Control | GraphemeBreak::Cr | GraphemeBreak::Lf)
        };
        let mut success = true;
        let mut checked = 0usize;
        'line: for (line_num, line) in content.lines().enumerate() {
            let trimmed = trim(line);
            if trimmed.is_empty() {
                continue;
            }
            let parts: Vec<&str> = trimmed.split(' ').collect();
            assert_eq!("÷", parts[0]);
            let mut state = BreakState::default();
            let first = u32::from_str_radix(parts[1], 16).unwrap();
            if is_control(grapheme_break(first)) {
                continue;
            }
            let mut i = 1;
            while i + 2 < parts.len() {
                let cp1 = u32::from_str_radix(parts[i], 16).unwrap();
                let expected_str = parts[i + 1];
                let cp2 = u32::from_str_radix(parts[i + 2], 16).unwrap();
                if is_control(grapheme_break(cp2)) {
                    continue 'line;
                }
                let gb1 = grapheme_break_no_control(cp1);
                let gb2 = grapheme_break_no_control(cp2);
                assert!(expected_str == "÷" || expected_str == "×");
                let mut expected_is_break = expected_str == "÷";
                let actual = actual_is_break(cp1, cp2, &mut state);
                if gb2 == GraphemeBreakNoControl::EmojiModifier
                    && gb1 != GraphemeBreakNoControl::EmojiModifierBase
                {
                    assert!(!expected_is_break);
                    expected_is_break = true;
                }
                if actual != expected_is_break {
                    eprintln!(
                        "line={} cp1={:x}, cp2={:x}: gb1={:?}, gb2={:?}, state={:?}, expected={}, actual={}",
                        line_num + 1, cp1, cp2, gb1, gb2, state, expected_is_break, actual
                    );
                    success = false;
                }
                checked += 1;
                i += 2;
            }
            assert_eq!("÷", parts[parts.len() - 1]);
        }
        assert!(checked > 1000);
        assert!(success);
    }

    fn trim(line: &str) -> &str {
        let line = match line.find('#') {
            Some(idx) => &line[..idx],
            None => line,
        };
        line.trim_matches(|c| c == ' ' || c == '\t' || c == '\r')
    }

    #[test]
    fn grapheme_break_test_compute_grapheme_break() {
        check_grapheme_break_test(|cp1, cp2, state| {
            compute_grapheme_break(grapheme_break(cp1), grapheme_break(cp2), state)
        });
    }

    #[test]
    fn grapheme_break_test_is_break() {
        check_grapheme_break_test(is_break);
    }

    #[test]
    fn grapheme_break_test_compute_grapheme_break_no_control() {
        check_grapheme_break_test_no_control(|cp1, cp2, state| {
            compute_grapheme_break_no_control(
                grapheme_break_no_control(cp1),
                grapheme_break_no_control(cp2),
                state,
            )
        });
    }

    #[test]
    fn grapheme_break_test_is_break_no_control() {
        check_grapheme_break_test_no_control(is_break_no_control);
    }
}
