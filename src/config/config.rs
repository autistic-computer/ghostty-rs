//! Port of `src/config/Config.zig`.
//!
//! STATUS: ported
//!
//! Config is the main config struct. These fields map directly to the
//! CLI flag names hence we use a lot of `as "..."` syntax to support hyphens.
//!
//! Pandoc is used to automatically generate manual pages and other forms of
//! documentation, so documentation comments on fields in the Config struct
//! should use Pandoc's flavor of Markdown.
//!
//! For a reference to Pandoc's Markdown see their [online
//! manual.](https://pandoc.org/MANUAL.html#pandocs-markdown)
//!
//! # Port notes
//!
//! * The struct is defined with [`cli_struct!`], which generates the field
//!   reflection Zig gets from `@typeInfo(Config)`: parsing
//!   ([`Fields::parse_into_field`]), formatting ([`FormatFields`]), change
//!   detection ([`ChangedFields`]) and the help strings ([`Fields::FIELDS`]).
//!   The field order is the order of [`Key`](super::key::Key).
//! * The arena (`_arena`) is dropped: every value owns its memory, so
//!   `deinit` is a no-op and `shallowClone` is a deep clone.
//! * `Config.default(alloc)` is the inherent [`Config::default`] (it adds the
//!   default keybinds, command palette entries and URL link). The derived
//!   [`Default`] trait impl only holds the raw field defaults (Zig `.{}`).
//! * `Config.clone(alloc)` is the inherent [`Config::clone`]; it shadows
//!   the derived `Clone::clone`.
//! * The replay iterator (`Replay.Iterator`) needs to mutate the config it
//!   is replaying into while `cli.args.parse` also holds it. In Rust the
//!   replay loop is [`Config::load_replay`], which feeds each replayed
//!   argument to `cli.args.parse` and handles `-e` itself.

use std::borrow::Cow;
use std::fmt::{self, Write as _};
use std::io::{self, BufRead, Read};

use crate::cli::args::{
    self, cli_enum, cli_flags, cli_struct, compatibility_renamed, impl_cli_enum, ArgIterator,
    ChangedFields, CliEnum, CompatibilityHandler, Error, Fields, FormatFields, ParseCli,
    ParseHooks,
};
use crate::cli::diagnostics::{Diagnostic, DiagnosticList, Location};
use crate::config::formatter::{self as formatterpkg, EntryFormatter, FormatEntry};
use crate::input::key::{ctrl_or_super, Mods};
use crate::terminal::color as terminal_color;
use crate::terminal::selection_codepoints;

use super::clipboard_codepoint_map::{self, ClipboardCodepointMap};
use super::conditional::{self, Conditional};
use super::file_load;
use super::string;
use super::theme as themepkg;
use super::url;

pub use crate::cli::args::StringZ;
pub use crate::input::config::OptionAsAlt;
pub use crate::input::key_mods::RemapSet as KeyRemapSet;
pub use crate::renderer::size::PaddingBalance as WindowPaddingBalance;
pub use crate::terminal::cursor::Style as CursorStyle;
pub use super::command::Command;
pub use super::io::RepeatableReadableIO;
pub use super::key::Key;
pub use super::limit::Limit;
pub use super::path::{Path, RepeatablePath};
pub use super::repeatable_string_map::RepeatableStringMap;

pub use crate::font::metrics::Modifier as MetricModifier;
pub use crate::build_config::ReleaseChannel;

impl_cli_enum!(ReleaseChannel {
    Tip => "tip",
    Stable => "stable",
});

/// Zig `std.time.ns_per_*`.
pub const NS_PER_US: u64 = 1000;
pub const NS_PER_MS: u64 = 1000 * NS_PER_US;
pub const NS_PER_S: u64 = 1000 * NS_PER_MS;
pub const NS_PER_MIN: u64 = 60 * NS_PER_S;
pub const NS_PER_HOUR: u64 = 60 * NS_PER_MIN;
pub const NS_PER_DAY: u64 = 24 * NS_PER_HOUR;
pub const NS_PER_WEEK: u64 = 7 * NS_PER_DAY;

/// Zig `std.Io.Dir.max_path_bytes`.
const MAX_PATH_BYTES: usize = 4096;

use crate::font::CodepointMap;
use crate::font::codepoint_map::Entry as CodepointMapEntry;
use crate::font::discovery::Descriptor;
use crate::font::face::{Variation, VariationId};

const LOG_TARGET: &str = "config";

/// Zig: `pub const compatibility = std.StaticStringMap(...)`.
pub fn compatibility(key: &str) -> Option<CompatibilityHandler<Config>> {
    match key {
        // Ghostty 1.1 introduced background-blur support for Linux which
        // doesn't support a specific radius value. The renaming is to let
        // one field be used for both platforms (macOS retained the ability
        // to set a radius).
        "background-blur-radius" => Some(compatibility_renamed!(Config, "background-blur")),

        // Ghostty 1.2 renamed all our adw options to gtk because we now have
        // a hard dependency on libadwaita.
        "adw-toolbar-style" => Some(compatibility_renamed!(Config, "gtk-toolbar-style")),

        // Ghostty 1.2 removed the `hidden` value from `gtk-tabs-location` and
        // moved it to `window-show-tab-bar`.
        "gtk-tabs-location" => Some(compat_gtk_tabs_location),

        // Ghostty 1.2 lets you set `cell-foreground` and `cell-background`
        // to match the cell foreground and background colors, respectively.
        // This can be used with `cursor-color` and `cursor-text` to recreate
        // this behavior. This applies to selection too.
        "cursor-invert-fg-bg" => Some(compat_cursor_invert_fg_bg),
        "selection-invert-fg-bg" => Some(compat_selection_invert_fg_bg),

        // Ghostty 1.2 merged `bold-is-bright` into the new `bold-color`
        // by setting the value to "bright".
        "bold-is-bright" => Some(compat_bold_is_bright),

        // Ghostty 1.2 removed the "desktop" option and renamed it to "detect".
        // The semantics also changed slightly but this is the correct mapping.
        "gtk-single-instance" => Some(compat_gtk_single_instance),

        // Ghostty 1.3 rename the "window" option to "new-window".
        // See: https://github.com/ghostty-org/ghostty/pull/9764
        "macos-dock-drop-behavior" => Some(compat_macos_dock_drop_behavior),

        // Ghostty 1.4 renamed `scrollback-limit` to `scrollback-limit-bytes`
        // when `scrollback-limit-lines` was added so the units are explicit.
        "scrollback-limit" => Some(compatibility_renamed!(Config, "scrollback-limit-bytes")),

        // Ghostty 1.4 updated "copy-on-select", allow copying to the selection
        // clipboard (on supported operating systems), the system clipboard, or
        // both. The semantics also changed but this is the correct mapping.
        // See: https://github.com/ghostty-org/ghostty/pull/12604
        "copy-on-select" => Some(compat_copy_on_select),

        _ => None,
    }
}

/// The conditional keys that are used at any point during the configuration
/// loading (Zig `std.EnumSet(conditional.Key)`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConditionalKeySet {
    bits: u64,
}

impl ConditionalKeySet {
    fn bit(key: conditional::Key) -> u64 {
        let idx = <conditional::Key as CliEnum>::FIELD_NAMES
            .iter()
            .position(|n| *n == key.tag_name())
            .expect("conditional key");
        1u64 << idx
    }

    pub fn insert(&mut self, key: conditional::Key) {
        self.bits |= Self::bit(key);
    }

    pub fn remove(&mut self, key: conditional::Key) {
        self.bits &= !Self::bit(key);
    }

    pub fn contains(&self, key: conditional::Key) -> bool {
        self.bits & Self::bit(key) != 0
    }
}

cli_struct! {
    /// Config is the main config struct. These fields map directly to the
    /// CLI flag names hence we use a lot of `@""` syntax to support hyphens.
    #[derive(Debug, Clone)]
    pub struct Config {
        /// Set Ghostty's graphical user interface language to a language other than the
        /// system default language. For example:
        ///
        ///     language = de
        ///
        /// will force the strings in Ghostty's graphical user interface to be in German
        /// rather than the system default.
        ///
        /// This will not affect the language used by programs run _within_ Ghostty.
        /// Those will continue to use the default system language. There are also many
        /// non-GUI elements in Ghostty that are not translated - this setting will have
        /// no effect on those.
        ///
        /// Warning: This setting cannot be reloaded at runtime. To change the language
        /// you must fully restart Ghostty.
        ///
        /// GTK only.
        /// Available since 1.3.0.
        pub language: Option<StringZ> = None,
        /// The font families to use.
        ///
        /// You can generate the list of valid values using the CLI:
        ///
        ///     ghostty +list-fonts
        ///
        /// This configuration can be repeated multiple times to specify preferred
        /// fallback fonts when the requested codepoint is not available in the primary
        /// font. This is particularly useful for multiple languages, symbolic fonts,
        /// etc.
        ///
        /// Notes on emoji specifically: On macOS, Ghostty by default will always use
        /// Apple Color Emoji and on Linux will always use Noto Emoji. You can
        /// override this behavior by specifying a font family here that contains
        /// emoji glyphs.
        ///
        /// The specific styles (bold, italic, bold italic) do not need to be
        /// explicitly set. If a style is not set, then the regular style (font-family)
        /// will be searched for stylistic variants. If a stylistic variant is not
        /// found, Ghostty will use the regular style. This prevents falling back to a
        /// different font family just to get a style such as bold. This also applies
        /// if you explicitly specify a font family for a style. For example, if you
        /// set `font-family-bold = FooBar` and "FooBar" cannot be found, Ghostty will
        /// use whatever font is set for `font-family` for the bold style.
        ///
        /// Finally, some styles may be synthesized if they are not supported.
        /// For example, if a font does not have an italic style and no alternative
        /// italic font is specified, Ghostty will synthesize an italic style by
        /// applying a slant to the regular style. If you want to disable these
        /// synthesized styles then you can use the `font-style` configurations
        /// as documented below.
        ///
        /// You can disable styles completely by using the `font-style` set of
        /// configurations. See the documentation for `font-style` for more information.
        ///
        /// If you want to overwrite a previous set value rather than append a fallback,
        /// specify the value as `""` (empty string) to reset the list and then set the
        /// new values. For example:
        ///
        ///     font-family = ""
        ///     font-family = "My Favorite Font"
        ///
        /// Setting any of these as CLI arguments will automatically clear the
        /// values set in configuration files so you don't need to specify
        /// `--font-family=""` before setting a new value. You only need to specify
        /// this within config files if you want to clear previously set values in
        /// configuration files or on the CLI if you want to clear values set on the
        /// CLI.
        pub font_family as "font-family": RepeatableString = RepeatableString::default(),
        pub font_family_bold as "font-family-bold": RepeatableString = RepeatableString::default(),
        pub font_family_italic as "font-family-italic": RepeatableString = RepeatableString::default(),
        pub font_family_bold_italic as "font-family-bold-italic": RepeatableString = RepeatableString::default(),
        /// The named font style to use for each of the requested terminal font styles.
        /// This looks up the style based on the font style string advertised by the
        /// font itself. For example, "Iosevka Heavy" has a style of "Heavy".
        ///
        /// You can also use these fields to completely disable a font style. If you set
        /// the value of the configuration below to literal `false` then that font style
        /// will be disabled. If the running program in the terminal requests a disabled
        /// font style, the regular font style will be used instead.
        ///
        /// These are only valid if its corresponding font-family is also specified. If
        /// no font-family is specified, then the font-style is ignored unless you're
        /// disabling the font style.
        pub font_style as "font-style": FontStyle = FontStyle::Default,
        pub font_style_bold as "font-style-bold": FontStyle = FontStyle::Default,
        pub font_style_italic as "font-style-italic": FontStyle = FontStyle::Default,
        pub font_style_bold_italic as "font-style-bold-italic": FontStyle = FontStyle::Default,
        /// Control whether Ghostty should synthesize a style if the requested style is
        /// not available in the specified font-family.
        ///
        /// Ghostty can synthesize bold, italic, and bold italic styles if the font
        /// does not have a specific style. For bold, this is done by drawing an
        /// outline around the glyph of varying thickness. For italic, this is done by
        /// applying a slant to the glyph. For bold italic, both of these are applied.
        ///
        /// Synthetic styles are not perfect and will generally not look as good
        /// as a font that has the style natively. However, they are useful to
        /// provide styled text when the font does not have the style.
        ///
        /// Set this to "false" or "true" to disable or enable synthetic styles
        /// completely. You can disable specific styles using "no-bold", "no-italic",
        /// and "no-bold-italic". You can disable multiple styles by separating them
        /// with a comma. For example, "no-bold,no-italic".
        ///
        /// Available style keys are: `bold`, `italic`, `bold-italic`.
        ///
        /// If synthetic styles are disabled, then the regular style will be used
        /// instead if the requested style is not available. If the font has the
        /// requested style, then the font will be used as-is since the style is
        /// not synthetic.
        ///
        /// Warning: An easy mistake is to disable `bold` or `italic` but not
        /// `bold-italic`. Disabling only `bold` or `italic` will NOT disable either
        /// in the `bold-italic` style. If you want to disable `bold-italic`, you must
        /// explicitly disable it. You cannot partially disable `bold-italic`.
        ///
        /// By default, synthetic styles are enabled.
        pub font_synthetic_style as "font-synthetic-style": FontSyntheticStyle = FontSyntheticStyle::default(),
        /// Apply a font feature. To enable multiple font features you can repeat
        /// this multiple times or use a comma-separated list of feature settings.
        ///
        /// The syntax for feature settings is as follows, where `feat` is a feature:
        ///
        ///   * Enable features with e.g. `feat`, `+feat`, `feat on`, `feat=1`.
        ///   * Disabled features with e.g. `-feat`, `feat off`, `feat=0`.
        ///   * Set a feature value with e.g. `feat=2`, `feat = 3`, `feat 4`.
        ///   * Feature names may be wrapped in quotes, meaning this config should be
        ///     syntactically compatible with the `font-feature-settings` CSS property.
        ///
        /// The syntax is fairly loose, but invalid settings will be silently ignored.
        ///
        /// The font feature will apply to all fonts rendered by Ghostty. A future
        /// enhancement will allow targeting specific faces.
        ///
        /// To disable programming ligatures, use `-calt` since this is the typical
        /// feature name for programming ligatures. To look into what font features
        /// your font has and what they do, use a font inspection tool such as
        /// [fontdrop.info](https://fontdrop.info).
        ///
        /// To generally disable most ligatures, use `-calt, -liga, -dlig`.
        pub font_feature as "font-feature": RepeatableString = RepeatableString::default(),
        /// Font size in points. This value can be a non-integer and the nearest integer
        /// pixel size will be selected. If you have a high dpi display where 1pt = 2px
        /// then you can get an odd numbered pixel size by specifying a half point.
        ///
        /// For example, 13.5pt @ 2px/pt = 27px
        ///
        /// Changing this configuration at runtime will only affect existing
        /// terminals that have NOT manually adjusted their font size in some way
        /// (e.g. increased or decreased the font size). Terminals that have manually
        /// adjusted their font size will retain their manually adjusted size.
        /// Otherwise, the font size of existing terminals will be updated on
        /// reload.
        ///
        /// On Linux with GTK, font size is scaled according to both display-wide and
        /// text-specific scaling factors, which are often managed by your desktop
        /// environment (e.g. the GNOME display scale and large text settings).
        pub font_size as "font-size": f32 = if cfg!(target_os = "macos") {
        // On macOS we default a little bigger since this tends to look better. This
        // is purely subjective but this is easy to modify.
        13.0
    } else {
        12.0
    },
        /// A repeatable configuration to set one or more font variations values for
        /// a variable font. A variable font is a single font, usually with a filename
        /// ending in `-VF.ttf` or `-VF.otf` that contains one or more configurable axes
        /// for things such as weight, slant, etc. Not all fonts support variations;
        /// only fonts that explicitly state they are variable fonts will work.
        ///
        /// The format of this is `id=value` where `id` is the axis identifier. An axis
        /// identifier is always a 4 character string, such as `wght`. To get the list
        /// of supported axes, look at your font documentation or use a font inspection
        /// tool.
        ///
        /// Invalid ids and values are usually ignored. For example, if a font only
        /// supports weights from 100 to 700, setting `wght=800` will do nothing (it
        /// will not be clamped to 700). You must consult your font's documentation to
        /// see what values are supported.
        ///
        /// Common axes are: `wght` (weight), `slnt` (slant), `ital` (italic), `opsz`
        /// (optical size), `wdth` (width), `GRAD` (gradient), etc.
        pub font_variation as "font-variation": RepeatableFontVariation = RepeatableFontVariation::default(),
        pub font_variation_bold as "font-variation-bold": RepeatableFontVariation = RepeatableFontVariation::default(),
        pub font_variation_italic as "font-variation-italic": RepeatableFontVariation = RepeatableFontVariation::default(),
        pub font_variation_bold_italic as "font-variation-bold-italic": RepeatableFontVariation = RepeatableFontVariation::default(),
        /// Force one or a range of Unicode codepoints to map to a specific named font.
        /// This is useful if you want to support special symbols or if you want to use
        /// specific glyphs that render better for your specific font.
        ///
        /// The syntax is `codepoint=fontname` where `codepoint` is either a single
        /// codepoint or a range. Codepoints must be specified as full Unicode
        /// hex values, such as `U+ABCD`. Codepoints ranges are specified as
        /// `U+ABCD-U+DEFG`. You can specify multiple ranges for the same font separated
        /// by commas, such as `U+ABCD-U+DEFG,U+1234-U+5678=fontname`. The font name is
        /// the same value as you would use for `font-family`.
        ///
        /// This configuration can be repeated multiple times to specify multiple
        /// codepoint mappings.
        ///
        /// Changing this configuration at runtime will only affect new terminals,
        /// i.e. new windows, tabs, etc.
        pub font_codepoint_map as "font-codepoint-map": RepeatableCodepointMap = RepeatableCodepointMap::default(),
        /// Map specific Unicode codepoints to replacement values when copying text
        /// to clipboard.
        ///
        /// This configuration allows you to replace specific Unicode characters with
        /// other characters or strings when copying terminal content to the clipboard.
        /// This is useful for converting special terminal symbols to more compatible
        /// characters for pasting into other applications.
        ///
        /// The syntax is similar to `font-codepoint-map`:
        /// - Single codepoint: `U+1234=U+ABCD` or `U+1234=replacement_text`
        /// - Codepoint range: `U+1234-U+5678=U+ABCD`
        ///
        /// Examples:
        /// - `clipboard-codepoint-map = U+2500=U+002D` (box drawing horizontal → hyphen)
        /// - `clipboard-codepoint-map = U+2502=U+007C` (box drawing vertical → pipe)
        /// - `clipboard-codepoint-map = U+03A3=SUM` (Greek sigma → "SUM")
        ///
        /// This configuration can be repeated multiple times to specify multiple
        /// mappings. Later entries take priority over earlier ones for overlapping
        /// ranges.
        ///
        /// Note: This only applies to text copying operations, not URL copying.
        pub clipboard_codepoint_map as "clipboard-codepoint-map": RepeatableClipboardCodepointMap = RepeatableClipboardCodepointMap::default(),
        /// Draw fonts with a thicker stroke, if supported.
        /// This is currently only supported on macOS.
        pub font_thicken as "font-thicken": bool = false,
        /// Strength of thickening when `font-thicken` is enabled.
        ///
        /// Valid values are integers between `0` and `255`. `0` does not correspond to
        /// *no* thickening, rather it corresponds to the lightest available thickening.
        ///
        /// Has no effect when `font-thicken` is set to `false`.
        ///
        /// This is currently only supported on macOS.
        pub font_thicken_strength as "font-thicken-strength": u8 = 255,
        /// Locations to break font shaping into multiple runs.
        ///
        /// A "run" is a contiguous segment of text that is shaped together. "Shaping"
        /// is the process of converting text (codepoints) into glyphs (renderable
        /// characters). This is how ligatures are formed, among other things.
        /// For example, if a coding font turns "!=" into a single glyph, then it
        /// must see "!" and "=" next to each other in a single run. When a run
        /// is broken, the text is shaped separately. To continue our example, if
        /// "!" is at the end of one run and "=" is at the start of the next run,
        /// then the ligature will not be formed.
        ///
        /// Ghostty breaks runs at certain points to improve readability or usability.
        /// For example, Ghostty by default will break runs under the cursor so that
        /// text editing can see the individual characters rather than a ligature.
        /// This configuration lets you configure this behavior.
        ///
        /// Combine values with a comma to set multiple options. Prefix an
        /// option with "no-" to disable it. Enabling and disabling options
        /// can be done at the same time.
        ///
        /// Available options:
        ///
        ///   * `cursor` - Break runs under the cursor.
        ///
        /// Available since: 1.2.0
        pub font_shaping_break as "font-shaping-break": FontShapingBreak = FontShapingBreak::default(),
        /// What color space to use when performing alpha blending.
        ///
        /// This affects the appearance of text and of any images with transparency.
        /// Additionally, custom shaders will receive colors in the configured space.
        ///
        /// On macOS the default is `native`, on all other platforms the default is
        /// `linear-corrected`.
        ///
        /// Valid values:
        ///
        /// * `native` - Perform alpha blending in the native color space for the OS.
        ///   On macOS this corresponds to Display P3, and on Linux it's sRGB.
        ///
        /// * `linear` - Perform alpha blending in linear space. This will eliminate
        ///   the darkening artifacts around the edges of text that are very visible
        ///   when certain color combinations are used (e.g. red / green), but makes
        ///   dark text look much thinner than normal and light text much thicker.
        ///   This is also sometimes known as "gamma correction".
        ///
        /// * `linear-corrected` - Same as `linear`, but with a correction step applied
        ///   for text that makes it look nearly or completely identical to `native`,
        ///   but without any of the darkening artifacts.
        ///
        /// Available since: 1.1.0
        pub alpha_blending as "alpha-blending": AlphaBlending = if cfg!(target_os = "macos") {
        AlphaBlending::Native
    } else {
        AlphaBlending::LinearCorrected
    },
        /// All of the configurations behavior adjust various metrics determined by the
        /// font. The values can be integers (1, -1, etc.) or a percentage (20%, -15%,
        /// etc.). In each case, the values represent the amount to change the original
        /// value.
        ///
        /// For example, a value of `1` increases the value by 1; it does not set it to
        /// literally 1. A value of `20%` increases the value by 20%. And so on.
        ///
        /// There is little to no validation on these values so the wrong values (e.g.
        /// `-100%`) can cause the terminal to be unusable. Use with caution and reason.
        ///
        /// Some values are clamped to minimum or maximum values. This can make it
        /// appear that certain values are ignored. For example, many `*-thickness`
        /// adjustments cannot go below 1px.
        ///
        /// `adjust-cell-height` has some additional behaviors to describe:
        ///
        ///   * The font will be centered vertically in the cell.
        ///
        ///   * The cursor will remain the same size as the font, but may be
        ///     adjusted separately with `adjust-cursor-height`.
        ///
        ///   * Powerline glyphs will be adjusted along with the cell height so
        ///     that things like status lines continue to look aligned.
        pub adjust_cell_width as "adjust-cell-width": Option<MetricModifier> = None,
        pub adjust_cell_height as "adjust-cell-height": Option<MetricModifier> = None,
        /// Distance in pixels or percentage adjustment from the bottom of the cell to the text baseline.
        /// Increase to move baseline UP, decrease to move baseline DOWN.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_font_baseline as "adjust-font-baseline": Option<MetricModifier> = None,
        /// Distance in pixels or percentage adjustment from the top of the cell to the top of the underline.
        /// Increase to move underline DOWN, decrease to move underline UP.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_underline_position as "adjust-underline-position": Option<MetricModifier> = None,
        /// Thickness in pixels of the underline.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_underline_thickness as "adjust-underline-thickness": Option<MetricModifier> = None,
        /// Distance in pixels or percentage adjustment from the top of the cell to the top of the strikethrough.
        /// Increase to move strikethrough DOWN, decrease to move strikethrough UP.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_strikethrough_position as "adjust-strikethrough-position": Option<MetricModifier> = None,
        /// Thickness in pixels or percentage adjustment of the strikethrough.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_strikethrough_thickness as "adjust-strikethrough-thickness": Option<MetricModifier> = None,
        /// Distance in pixels or percentage adjustment from the top of the cell to the top of the overline.
        /// Increase to move overline DOWN, decrease to move overline UP.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_overline_position as "adjust-overline-position": Option<MetricModifier> = None,
        /// Thickness in pixels or percentage adjustment of the overline.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_overline_thickness as "adjust-overline-thickness": Option<MetricModifier> = None,
        /// Thickness in pixels or percentage adjustment of the bar cursor and outlined rect cursor.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_cursor_thickness as "adjust-cursor-thickness": Option<MetricModifier> = None,
        /// Height in pixels or percentage adjustment of the cursor. Currently applies to all cursor types:
        /// bar, rect, and outlined rect.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_cursor_height as "adjust-cursor-height": Option<MetricModifier> = None,
        /// Thickness in pixels or percentage adjustment of box drawing characters.
        /// See the notes about adjustments in `adjust-cell-width`.
        pub adjust_box_thickness as "adjust-box-thickness": Option<MetricModifier> = None,
        /// Height in pixels or percentage adjustment of maximum height for nerd font icons.
        ///
        /// A positive (negative) value will increase (decrease) the maximum icon
        /// height. This may not affect all icons equally: the effect depends on whether
        /// the default size of the icon is height-constrained, which in turn depends on
        /// the aspect ratio of both the icon and your primary font.
        ///
        /// Certain icons designed for box drawing and terminal graphics, such as
        /// Powerline symbols, are not affected by this option.
        ///
        /// See the notes about adjustments in `adjust-cell-width`.
        ///
        /// Available in: 1.2.0
        pub adjust_icon_height as "adjust-icon-height": Option<MetricModifier> = None,
        /// The method to use for calculating the cell width of a grapheme cluster.
        /// The default value is `unicode` which uses the Unicode standard to determine
        /// grapheme width. This results in correct grapheme width but may result in
        /// cursor-desync issues with some programs (such as shells) that may use a
        /// legacy method such as `wcswidth`.
        ///
        /// Valid values are:
        ///
        /// * `legacy` - Use a legacy method to determine grapheme width, such as
        ///   wcswidth This maximizes compatibility with legacy programs but may result
        ///   in incorrect grapheme width for certain graphemes such as skin-tone
        ///   emoji, non-English characters, etc.
        ///
        ///   This is called "legacy" and not something more specific because the
        ///   behavior is undefined and we want to retain the ability to modify it.
        ///   For example, we may or may not use libc `wcswidth` now or in the future.
        ///
        /// * `unicode` - Use the Unicode standard to determine grapheme width.
        ///
        /// If a running program explicitly enables terminal mode 2027, then `unicode`
        /// width will be forced regardless of this configuration. When mode 2027 is
        /// reset, this configuration will be used again.
        ///
        /// This configuration can be changed at runtime but will not affect existing
        /// terminals. Only new terminals will use the new configuration.
        pub grapheme_width_method as "grapheme-width-method": GraphemeWidthMethod = GraphemeWidthMethod::Unicode,
        /// FreeType load flags to enable. The format of this is a list of flags to
        /// enable separated by commas. If you prefix a flag with `no-` then it is
        /// disabled. If you omit a flag, its default value is used, so you must
        /// explicitly disable flags you don't want. You can also use `true` or `false`
        /// to turn all flags on or off.
        ///
        /// This configuration only applies to Ghostty builds that use FreeType.
        /// This is usually the case only for Linux builds. macOS uses CoreText
        /// and does not have an equivalent configuration.
        ///
        /// Available flags:
        ///
        ///   * `hinting` - Enable or disable hinting. Enabled by default.
        ///
        ///   * `force-autohint` - Always use the freetype auto-hinter instead of
        ///     the font's native hinter. Disabled by default.
        ///
        ///   * `monochrome` - Instructs renderer to use 1-bit monochrome rendering.
        ///     This will disable anti-aliasing, and probably not look very good unless
        ///     you're using a pixel font. Disabled by default.
        ///
        ///   * `autohint` - Enable the freetype auto-hinter. Enabled by default.
        ///
        ///   * `light` - Use a light hinting style, better preserving glyph shapes.
        ///     This is the most common setting in GTK apps and therefore also Ghostty's
        ///     default. This has no effect if `monochrome` is enabled. Enabled by
        ///     default.
        ///
        /// Example: `hinting`, `no-hinting`, `force-autohint`, `no-force-autohint`
        pub freetype_load_flags as "freetype-load-flags": FreetypeLoadFlags = FreetypeLoadFlags::default(),
        /// A theme to use. This can be a built-in theme name, a custom theme
        /// name, or an absolute path to a custom theme file. Ghostty also supports
        /// specifying a different theme to use for light and dark mode. Each
        /// option is documented below.
        ///
        /// If the theme is an absolute pathname, Ghostty will attempt to load that
        /// file as a theme. If that file does not exist or is inaccessible, an error
        /// will be logged and no other directories will be searched.
        ///
        /// If the theme is not an absolute pathname, two different directories will be
        /// searched for a file name that matches the theme. This is case sensitive on
        /// systems with case-sensitive filesystems. It is an error for a theme name to
        /// include path separators unless it is an absolute pathname.
        ///
        /// The first directory is the `themes` subdirectory of your Ghostty
        /// configuration directory. This is `$XDG_CONFIG_HOME/ghostty/themes` or
        /// `~/.config/ghostty/themes`.
        ///
        /// The second directory is the `themes` subdirectory of the Ghostty resources
        /// directory. Ghostty ships with a multitude of themes that will be installed
        /// into this directory. On macOS, this list is in the
        /// `Ghostty.app/Contents/Resources/ghostty/themes` directory. On Linux, this
        /// list is in the `share/ghostty/themes` directory (wherever you installed the
        /// Ghostty "share" directory.
        ///
        /// To see a list of available themes, run `ghostty +list-themes`.
        ///
        /// A theme file is simply another Ghostty configuration file. They share
        /// the same syntax and same configuration options. A theme can set any valid
        /// configuration option so please do not use a theme file from an untrusted
        /// source. The built-in themes are audited to only set safe configuration
        /// options.
        ///
        /// Some options cannot be set within theme files. The reason these are not
        /// supported should be self-evident. A theme file cannot set `theme` or
        /// `config-file`. At the time of writing this, Ghostty will not show any
        /// warnings or errors if you set these options in a theme file but they will
        /// be silently ignored.
        ///
        /// Any additional colors specified via background, foreground, palette, etc.
        /// will override the colors specified in the theme.
        ///
        /// To specify a different theme for light and dark mode, use the following
        /// syntax: `light:theme-name,dark:theme-name`. For example:
        /// `light:Rose Pine Dawn,dark:Rose Pine`. Whitespace around all values are
        /// trimmed and order of light and dark does not matter. Both light and dark
        /// must be specified in this form. In this form, the theme used will be
        /// based on the current desktop environment theme.
        ///
        /// There are some known bugs with light/dark mode theming. These will
        /// be fixed in a future update:
        ///
        ///   - macOS: titlebar tabs style is not updated when switching themes.
        pub theme: Option<Theme> = None,
        /// Background color for the window.
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        pub background: Color = Color { r: 0x28, g: 0x2C, b: 0x34 },
        /// Foreground color for the window.
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        pub foreground: Color = Color { r: 0xFF, g: 0xFF, b: 0xFF },
        /// Background image for the terminal.
        ///
        /// This should be a path to a PNG or JPEG file, other image formats are
        /// not yet supported.
        ///
        /// The background image is currently per-terminal, not per-window. If
        /// you are a heavy split user, the background image will be repeated across
        /// splits. A future improvement to Ghostty will address this.
        ///
        /// WARNING: Background images are currently duplicated in VRAM per-terminal.
        /// For sufficiently large images, this could lead to a large increase in
        /// memory usage (specifically VRAM usage). A future Ghostty improvement
        /// will resolve this by sharing image textures across terminals.
        ///
        /// Available since: 1.2.0
        pub background_image as "background-image": Option<Path> = None,
        /// Background image opacity.
        ///
        /// This is relative to the value of `background-opacity`.
        ///
        /// A value of `1.0` (the default) will result in the background image being
        /// placed on top of the general background color, and then the combined result
        /// will be adjusted to the opacity specified by `background-opacity`.
        ///
        /// A value less than `1.0` will result in the background image being mixed
        /// with the general background color before the combined result is adjusted
        /// to the configured `background-opacity`.
        ///
        /// A value greater than `1.0` will result in the background image having a
        /// higher opacity than the general background color. For instance, if the
        /// configured `background-opacity` is `0.5` and `background-image-opacity`
        /// is set to `1.5`, then the final opacity of the background image will be
        /// `0.5 * 1.5 = 0.75`.
        ///
        /// Available since: 1.2.0
        pub background_image_opacity as "background-image-opacity": f32 = 1.0,
        /// Background image position.
        ///
        /// Valid values are:
        ///   * `top-left`
        ///   * `top-center`
        ///   * `top-right`
        ///   * `center-left`
        ///   * `center`
        ///   * `center-right`
        ///   * `bottom-left`
        ///   * `bottom-center`
        ///   * `bottom-right`
        ///
        /// The default value is `center`.
        ///
        /// Available since: 1.2.0
        pub background_image_position as "background-image-position": BackgroundImagePosition = BackgroundImagePosition::Center,
        /// Background image fit.
        ///
        /// Valid values are:
        ///
        ///  * `contain`
        ///
        ///    Preserving the aspect ratio, scale the background image to the largest
        ///    size that can still be contained within the terminal, so that the whole
        ///    image is visible.
        ///
        ///  * `cover`
        ///
        ///    Preserving the aspect ratio, scale the background image to the smallest
        ///    size that can completely cover the terminal. This may result in one or
        ///    more edges of the image being clipped by the edge of the terminal.
        ///
        ///  * `stretch`
        ///
        ///    Stretch the background image to the full size of the terminal, without
        ///    preserving the aspect ratio.
        ///
        ///  * `none`
        ///
        ///    Don't scale the background image.
        ///
        /// The default value is `contain`.
        ///
        /// Available since: 1.2.0
        pub background_image_fit as "background-image-fit": BackgroundImageFit = BackgroundImageFit::Contain,
        /// Whether to repeat the background image or not.
        ///
        /// If this is set to true, the background image will be repeated if there
        /// would otherwise be blank space around it because it doesn't completely
        /// fill the terminal area.
        ///
        /// The default value is `false`.
        ///
        /// Available since: 1.2.0
        pub background_image_repeat as "background-image-repeat": bool = false,
        /// The foreground and background color for selection. If this is not set, then
        /// the selection color is just the inverted window background and foreground
        /// (note: not to be confused with the cell bg/fg).
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        /// Since version 1.2.0, this can also be set to `cell-foreground` to match
        /// the cell foreground color, or `cell-background` to match the cell
        /// background color.
        pub selection_foreground as "selection-foreground": Option<TerminalColor> = None,
        pub selection_background as "selection-background": Option<TerminalColor> = None,
        /// Whether to clear selected text when typing. This defaults to `true`.
        /// This is typical behavior for most terminal emulators as well as
        /// text input fields. If you set this to `false`, then the selected text
        /// will not be cleared when typing.
        ///
        /// "Typing" is specifically defined as any non-modifier (shift, control,
        /// alt, etc.) keypress that produces data to be sent to the application
        /// running within the terminal (e.g. the shell). Additionally, selection
        /// is cleared when any preedit or composition state is started (e.g.
        /// when typing languages such as Japanese).
        ///
        /// If this is `false`, then the selection can still be manually
        /// cleared by clicking once or by pressing `escape`.
        ///
        /// Available since: 1.2.0
        pub selection_clear_on_typing as "selection-clear-on-typing": bool = true,
        /// Whether to clear selected text after copying. This defaults to `false`.
        ///
        /// When set to `true`, the selection will be automatically cleared after
        /// any copy operation that invokes the `copy_to_clipboard` keyboard binding.
        /// Importantly, this will not clear the selection if the copy operation
        /// was invoked via `copy-on-select`.
        ///
        /// When set to `false`, the selection remains visible after copying, allowing
        /// to see what was copied and potentially perform additional operations
        /// on the same selection.
        pub selection_clear_on_copy as "selection-clear-on-copy": bool = false,
        /// Characters that mark word boundaries during text selection operations such
        /// as double-clicking. When selecting a word, the selection will stop at any
        /// of these characters.
        ///
        /// This is similar to the `WORDCHARS` environment variable in zsh, except this
        /// specifies the boundary characters rather than the word characters. The
        /// default includes common delimiters and punctuation that typically separate
        /// words in code and prose.
        ///
        /// Each character in this string becomes a word boundary. Multi-byte UTF-8
        /// characters are supported, but only single codepoints can be specified.
        /// Multi-codepoint sequences (e.g. emoji) are not supported.
        ///
        /// The null character (U+0000) is always treated as a boundary and does not
        /// need to be included in this configuration.
        ///
        /// Default: ``\t '"│`|:;,()[]{}<>$``
        ///
        /// To add or remove specific characters, you can set this to a custom value.
        /// For example, to treat semicolons as part of words:
        ///
        ///     selection-word-chars = "\t '\"│`|:,()[]{}<>$"
        ///
        /// Available since: 1.3.0
        pub selection_word_chars as "selection-word-chars": SelectionWordChars = SelectionWordChars::default(),
        /// The minimum contrast ratio between the foreground and background colors.
        /// The contrast ratio is a value between 1 and 21. A value of 1 allows for no
        /// contrast (e.g. black on black). This value is the contrast ratio as defined
        /// by the [WCAG 2.0 specification](https://www.w3.org/TR/WCAG20/).
        ///
        /// If you want to avoid invisible text (same color as background), a value of
        /// 1.1 is a good value. If you want to avoid text that is difficult to read, a
        /// value of 3 or higher is a good value. The higher the value, the more likely
        /// that text will become black or white.
        ///
        /// This value does not apply to Emoji or images.
        pub minimum_contrast as "minimum-contrast": f64 = 1.0,
        /// Color palette for the 256 color form that many terminal applications use.
        /// The syntax of this configuration is `N=COLOR` where `N` is 0 to 255 (for
        /// the 256 colors in the terminal color table) and `COLOR` is a typical RGB
        /// color code such as `#AABBCC` or `AABBCC`, or a named X11 color. For example,
        /// `palette = 5=#BB78D9` will set the 'purple' color.
        ///
        /// The palette index can be in decimal, binary, octal, or hexadecimal.
        /// Decimal is assumed unless a prefix is used: `0b` for binary, `0o` for octal,
        /// and `0x` for hexadecimal.
        ///
        /// For definitions on the color indices and what they canonically map to,
        /// [see this cheat sheet](https://www.ditig.com/256-colors-cheat-sheet).
        ///
        /// For most themes, you only need to set the first 16 colors (0–15) since the
        /// rest of the palette (16–255) will be automatically generated by
        /// default (see `palette-generate` for more details).
        pub palette: Palette = Palette::default(),
        /// Whether to automatically generate the extended 256 color palette
        /// (indices 16–255) from the base 16 ANSI colors.
        ///
        /// This lets theme authors specify only the base 16 colors and have the
        /// rest of the palette be automatically generated in a consistent and
        /// aesthetic way.
        ///
        /// When enabled, the 6×6×6 color cube and 24-step grayscale ramp are
        /// derived from interpolations of the base palette, giving a more cohesive
        /// look. Colors that have been explicitly set via `palette` are never
        /// overwritten.
        ///
        /// The default value is false (disabled), because many legacy programs
        /// using the 256-color palette hardcode assumptions about what these
        /// colors are (mostly assuming the xterm 256 color palette). However, this
        /// is still a very useful tool for theme authors and users who want
        /// to customize their palette without having to specify all 256 colors.
        ///
        /// For more information on how the generation works, see here:
        /// https://gist.github.com/jake-stewart/0a8ea46159a7da2c808e5be2177e1783
        ///
        /// Available since: 1.3.0
        pub palette_generate as "palette-generate": bool = false,
        /// Invert the palette colors generated when `palette-generate` is enabled,
        /// so that the colors go in reverse order. This allows palette-based
        /// applications to work well in both light and dark mode since the
        /// palettes are always relatively good colors.
        ///
        /// This defaults to off because some legacy terminal applications
        /// hardcode the assumption that palette indices 16–231 are ordered from
        /// darkest to lightest, so enabling this would make them unreadable.
        /// This is not a generally good assumption and we encourage modern
        /// terminal applications to use the indices in a more semantic way.
        ///
        /// This has no effect if `palette-generate` is disabled.
        ///
        /// For more information see `palette-generate`.
        ///
        /// Available since: 1.3.0
        pub palette_harmonious as "palette-harmonious": bool = false,
        /// The color of the cursor. If this is not set, a default will be chosen.
        ///
        /// Direct colors can be specified as either hex (`#RRGGBB` or `RRGGBB`)
        /// or a named X11 color.
        ///
        /// Additionally, special values can be used to set the color to match
        /// other colors at runtime:
        ///
        ///   * `cell-foreground` - Match the cell foreground color.
        ///     (Available since: 1.2.0)
        ///
        ///   * `cell-background` - Match the cell background color.
        ///     (Available since: 1.2.0)
        pub cursor_color as "cursor-color": Option<TerminalColor> = None,
        /// The opacity level (opposite of transparency) of the cursor. A value of 1
        /// is fully opaque and a value of 0 is fully transparent. A value less than 0
        /// or greater than 1 will be clamped to the nearest valid value. Note that a
        /// sufficiently small value such as 0.3 may be effectively invisible and may
        /// make it difficult to find the cursor.
        pub cursor_opacity as "cursor-opacity": f64 = 1.0,
        /// The style of the cursor. This sets the default style. A running program can
        /// still request an explicit cursor style using escape sequences (such as `CSI
        /// q`). Shell configurations will often request specific cursor styles.
        ///
        /// Note that shell integration will automatically set the cursor to a bar at
        /// a prompt, regardless of this configuration. You can disable that behavior
        /// by specifying `shell-integration-features = no-cursor` or disabling shell
        /// integration entirely.
        ///
        /// Valid values are:
        ///
        ///   * `block`
        ///   * `bar`
        ///   * `underline`
        ///   * `block_hollow`
        pub cursor_style as "cursor-style": CursorStyle = CursorStyle::Block,
        /// Sets the default blinking state of the cursor. This is just the default
        /// state; running programs may override the cursor style using `DECSCUSR` (`CSI
        /// q`).
        ///
        /// If this is not set, the cursor blinks by default. Note that this is not the
        /// same as a "true" value, as noted below.
        ///
        /// If this is not set at all (`null`), then Ghostty will respect DEC Mode 12
        /// (AT&T cursor blink) as an alternate approach to turning blinking on/off. If
        /// this is set to any value other than null, DEC mode 12 will be ignored but
        /// `DECSCUSR` will still be respected.
        ///
        /// Valid values are:
        ///
        ///   * ` ` (blank)
        ///   * `true`
        ///   * `false`
        pub cursor_style_blink as "cursor-style-blink": Option<bool> = None,
        /// The color of the text under the cursor. If this is not set, a default will
        /// be chosen.
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        /// Since version 1.2.0, this can also be set to `cell-foreground` to match
        /// the cell foreground color, or `cell-background` to match the cell
        /// background color.
        pub cursor_text as "cursor-text": Option<TerminalColor> = None,
        /// Enables the ability to move the cursor at prompts by clicking on a
        /// location in the prompt text.
        ///
        /// This feature requires prompt marking via `OSC 133`. Some shells like Fish
        /// (v4.1+) and Nu (0.111+) natively support this while others may require
        /// additional configuration or Ghostty's shell integration features to be
        /// enabled.
        ///
        /// Depending on the shell, this works either by translating your click
        /// position into a series of synthetic arrow key movements or by sending
        /// a click event directly to the shell. In either case, some unexpected
        /// behavior around edge cases is possible.
        pub cursor_click_to_move as "cursor-click-to-move": bool = true,
        /// Hide the mouse immediately when typing. The mouse becomes visible again
        /// when the mouse is used (button, movement, etc.). Platform-specific behavior
        /// may dictate other scenarios where the mouse is shown. For example on macOS,
        /// the mouse is shown again when a new window, tab, or split is created.
        pub mouse_hide_while_typing as "mouse-hide-while-typing": bool = false,
        /// When to scroll the surface to the bottom. The format of this is a list of
        /// options to enable separated by commas. If you prefix an option with `no-`
        /// then it is disabled. If you omit an option, its default value is used.
        ///
        /// Available options:
        ///
        /// - `keystroke` If set, scroll the surface to the bottom when the user
        ///   presses a key that results in data being sent to the PTY (basically
        ///   anything but modifiers or keybinds that are processed by Ghostty).
        ///
        /// - `output` If set, scroll the surface to the bottom if there is new data
        ///   to display (e.g., when new lines are printed to the terminal).
        ///
        /// The default is `keystroke, no-output`.
        pub scroll_to_bottom as "scroll-to-bottom": ScrollToBottom = ScrollToBottom::default(),
        /// Determines whether running programs can detect the shift key pressed with a
        /// mouse click. Typically, the shift key is used to extend mouse selection.
        ///
        /// The default value of `false` means that the shift key is not sent with
        /// the mouse protocol and will extend the selection. This value can be
        /// conditionally overridden by the running program with the `XTSHIFTESCAPE`
        /// sequence.
        ///
        /// The value `true` means that the shift key is sent with the mouse protocol
        /// but the running program can override this behavior with `XTSHIFTESCAPE`.
        ///
        /// The value `never` is the same as `false` but the running program cannot
        /// override this behavior with `XTSHIFTESCAPE`. The value `always` is the
        /// same as `true` but the running program cannot override this behavior with
        /// `XTSHIFTESCAPE`.
        ///
        /// If you always want shift to extend mouse selection even if the program
        /// requests otherwise, set this to `never`.
        ///
        /// Valid values are:
        ///
        ///   * `true`
        ///   * `false`
        ///   * `always`
        ///   * `never`
        pub mouse_shift_capture as "mouse-shift-capture": MouseShiftCapture = MouseShiftCapture::False,
        /// Enable or disable mouse reporting. When set to `false`, mouse events will
        /// not be reported to terminal applications even if they request it. This
        /// allows you to always use the mouse for selection and other terminal UI
        /// interactions without applications capturing mouse input.
        ///
        /// When set to `true` (the default), terminal applications can request mouse
        /// reporting and will receive mouse events according to their requested mode.
        ///
        /// This can be toggled at runtime using the `toggle_mouse_reporting` keybind
        /// action.
        pub mouse_reporting as "mouse-reporting": bool = true,
        /// Multiplier for scrolling distance with the mouse wheel.
        ///
        /// A prefix of `precision:` or `discrete:` can be used to set the multiplier
        /// only for scrolling with the specific type of devices. These can be
        /// comma-separated to set both types of multipliers at the same time, e.g.
        /// `precision:0.1,discrete:3`. If no prefix is used, the multiplier applies
        /// to all scrolling devices. Specifying a prefix was introduced in Ghostty
        /// 1.2.1.
        ///
        /// The value will be clamped to [0.01, 10,000]. Both of these are extreme
        /// and you're likely to have a bad experience if you set either extreme.
        ///
        /// The default value is "3" for discrete devices and "1" for precision devices.
        pub mouse_scroll_multiplier as "mouse-scroll-multiplier": MouseScrollMultiplier = MouseScrollMultiplier::default(),
        /// The opacity level (opposite of transparency) of the background. A value of
        /// 1 is fully opaque and a value of 0 is fully transparent. A value less than 0
        /// or greater than 1 will be clamped to the nearest valid value.
        ///
        /// On macOS, background opacity is disabled when the terminal enters native
        /// fullscreen. This is because the background becomes gray and it can cause
        /// widgets to show through which isn't generally desirable.
        ///
        /// On macOS, changing this configuration requires restarting Ghostty completely.
        pub background_opacity as "background-opacity": f64 = 1.0,
        /// Applies background opacity to cells with an explicit background color
        /// set.
        ///
        /// Normally, `background-opacity` is only applied to the window background.
        /// If a cell has an explicit background color set, such as red, then that
        /// background color will be fully opaque. An effect of this is that some
        /// terminal applications that repaint the background color of the terminal
        /// such as a Neovim and Tmux may not respect the `background-opacity`
        /// (by design).
        ///
        /// Setting this to `true` will apply the `background-opacity` to all cells
        /// regardless of whether they have an explicit background color set or not.
        ///
        /// Available since: 1.2.0
        pub background_opacity_cells as "background-opacity-cells": bool = false,
        /// Whether to blur the background when `background-opacity` is less than 1.
        ///
        /// Valid values are:
        ///
        ///   * a nonnegative integer specifying the *blur intensity*
        ///   * `false`, equivalent to a blur intensity of 0
        ///   * `true`, equivalent to the default blur intensity of 20, which is
        ///     reasonable for a good looking blur. Higher blur intensities may
        ///     cause strange rendering and performance issues.
        ///
        /// On macOS 26.0 and later, there are additional special values that
        /// can be set to use the native macOS glass effects:
        ///
        ///   * `macos-glass-regular` - Standard glass effect with some opacity
        ///   * `macos-glass-clear` - Highly transparent glass effect
        ///
        /// If the macOS values are set, then this implies `background-blur = true`
        /// on non-macOS platforms.
        ///
        /// On Linux, the exact blur intensity is ignored and any positive integer
        /// or `true` value will enable background blur. On Wayland, Ghostty uses
        /// the [`ext-background-effect-v1`](https://wayland.app/protocols/ext-background-effect-v1)
        /// protocol to enable the blur effect, but this is not always supported
        /// on all compositors. Check the documentation of your compositor or
        /// desktop environment for blur support and how to configure it.
        ///
        /// On X11, blur can only be enabled when using the KWin compositor
        /// as a part of KDE Plasma.
        pub background_blur as "background-blur": BackgroundBlur = BackgroundBlur::False,
        /// The opacity level (opposite of transparency) of an unfocused split.
        /// Unfocused splits by default are slightly faded out to make it easier to see
        /// which split is focused. To disable this feature, set this value to 1.
        ///
        /// A value of 1 is fully opaque and a value of 0 is fully transparent. Because
        /// "0" is not useful (it makes the window look very weird), the minimum value
        /// is 0.15. This value still looks weird but you can at least see what's going
        /// on. A value outside of the range 0.15 to 1 will be clamped to the nearest
        /// valid value.
        pub unfocused_split_opacity as "unfocused-split-opacity": f64 = 0.7,
        /// The color to dim the unfocused split. Unfocused splits are dimmed by
        /// rendering a semi-transparent rectangle over the split. This sets the color of
        /// that rectangle and can be used to carefully control the dimming effect.
        ///
        /// This will default to the background color.
        ///
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        pub unfocused_split_fill as "unfocused-split-fill": Option<Color> = None,
        /// The color of the split divider. If this is not set, a default will be chosen.
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        ///
        /// Available since: 1.1.0
        pub split_divider_color as "split-divider-color": Option<Color> = None,
        /// Control when Ghostty preserves a zoomed split. Under normal circumstances,
        /// any operation that changes focus or layout of the split tree in a window
        /// will unzoom any zoomed split. This configuration allows you to control
        /// this behavior.
        ///
        /// This can be set to `navigation` to preserve the zoomed split state
        /// when navigating to another split (e.g. via `goto_split`). This will
        /// change the zoomed split to the newly focused split instead of unzooming.
        ///
        /// Any options can also be prefixed with `no-` to disable that option.
        ///
        /// Example: `split-preserve-zoom = navigation`
        ///
        /// Available since: 1.3.0
        pub split_preserve_zoom as "split-preserve-zoom": SplitPreserveZoom = SplitPreserveZoom::default(),
        /// The foreground and background color for search matches. This only applies
        /// to non-focused search matches, also known as candidate matches.
        ///
        /// Valid values:
        ///
        ///   - Hex (`#RRGGBB` or `RRGGBB`)
        ///   - Named X11 color
        ///   - "cell-foreground" to match the cell foreground color
        ///   - "cell-background" to match the cell background color
        ///
        /// The default value is black text on a golden yellow background.
        pub search_foreground as "search-foreground": TerminalColor = TerminalColor::Color(Color { r: 0, g: 0, b: 0 }),
        pub search_background as "search-background": TerminalColor = TerminalColor::Color(Color { r: 0xFF, g: 0xE0, b: 0x82 }),
        /// The foreground and background color for the currently selected search match.
        /// This is the focused match that will be jumped to when using next/previous
        /// search navigation.
        ///
        /// Valid values:
        ///
        ///   - Hex (`#RRGGBB` or `RRGGBB`)
        ///   - Named X11 color
        ///   - "cell-foreground" to match the cell foreground color
        ///   - "cell-background" to match the cell background color
        ///
        /// The default value is black text on a soft peach background.
        pub search_selected_foreground as "search-selected-foreground": TerminalColor = TerminalColor::Color(Color { r: 0, g: 0, b: 0 }),
        pub search_selected_background as "search-selected-background": TerminalColor = TerminalColor::Color(Color { r: 0xF2, g: 0xA5, b: 0x7E }),
        /// The command to run, usually a shell. If this is not an absolute path, it'll
        /// be looked up in the `PATH`. If this is not set, a default will be looked up
        /// from your system. The rules for the default lookup are:
        ///
        ///   * `SHELL` environment variable
        ///
        ///   * `passwd` entry (user information)
        ///
        /// This can contain additional arguments to run the command with. If additional
        /// arguments are provided, the command will be executed using `/bin/sh -c`
        /// to offload shell argument expansion.
        ///
        /// To avoid shell expansion altogether, prefix the command with `direct:`, e.g.
        /// `direct:nvim foo`. This will avoid the roundtrip to `/bin/sh` but will also
        /// not support any shell parsing such as arguments with spaces, filepaths with
        /// `~`, globs, etc. (Available since: 1.2.0)
        ///
        /// You can also explicitly prefix the command with `shell:` to always wrap the
        /// command in a shell. This can be used to ensure our heuristics to choose the
        /// right mode are not used in case they are wrong. (Available since: 1.2.0)
        ///
        /// This command will be used for all new terminal surfaces, i.e. new windows,
        /// tabs, etc. If you want to run a command only for the first terminal surface
        /// created when Ghostty starts, use the `initial-command` configuration.
        ///
        /// Ghostty supports the common `-e` flag for executing a command with
        /// arguments. For example, `ghostty -e fish --with --custom --args`.
        /// This flag sets the `initial-command` configuration, see that for more
        /// information.
        pub command: Option<Command> = None,
        /// This is the same as "command", but only applies to the first terminal
        /// surface created when Ghostty starts. Subsequent terminal surfaces will use
        /// the `command` configuration.
        ///
        /// After the first terminal surface is created (or closed), there is no
        /// way to run this initial command again automatically. As such, setting
        /// this at runtime works but will only affect the next terminal surface
        /// if it is the first one ever created.
        ///
        /// If you're using the `ghostty` CLI there is also a shortcut to set this
        /// with arguments directly: you can use the `-e` flag. For example: `ghostty -e
        /// fish --with --custom --args`. The `-e` flag automatically forces some
        /// other behaviors as well:
        ///
        ///   * Disables shell expansion since the input is expected to already
        ///     be shell-expanded by the upstream (e.g. the shell used to type in
        ///     the `ghostty -e` command).
        ///
        ///   * `gtk-single-instance=false` - This ensures that a new instance is
        ///     launched and the CLI args are respected.
        ///
        ///   * `quit-after-last-window-closed=true` - This ensures that the Ghostty
        ///     process will exit when the command exits. Additionally, the
        ///     `quit-after-last-window-closed-delay` is unset.
        ///
        ///   * `shell-integration=detect` (if not `none`) - This prevents forcibly
        ///     injecting any configured shell integration into the command's
        ///     environment. With `-e` its highly unlikely that you're executing a
        ///     shell and forced shell integration is likely to cause problems
        ///     (e.g. by wrapping your command in a shell, setting env vars, etc.).
        ///     This is a safety measure to prevent unexpected behavior. If you want
        ///     shell integration with a `-e`-executed command, you must either
        ///     name your binary appropriately or source the shell integration script
        ///     manually.
        pub initial_command as "initial-command": Option<Command> = None,
        /// Controls when command finished notifications are sent. There are
        /// three options:
        ///
        /// * `never` - Never send notifications (the default).
        /// * `unfocused` - Only send notifications if the surface that the command is
        ///   running in is not focused.
        /// * `always` - Always send notifications.
        ///
        /// Command finished notifications requires that either shell integration is
        /// enabled, or that your shell sends OSC 133 escape sequences to mark the start
        /// and end of commands.
        ///
        /// On GTK, there is a context menu item that will enable command finished
        /// notifications for a single command, overriding the `never` and `unfocused`
        /// options.
        ///
        /// Available since 1.3.0.
        pub notify_on_command_finish as "notify-on-command-finish": NotifyOnCommandFinish = NotifyOnCommandFinish::Never,
        /// If command finished notifications are enabled, this controls how the user is
        /// notified.
        ///
        /// Available options:
        ///
        /// * `bell` - enabled by default
        /// * `notify` - disabled by default
        ///
        /// Options can be combined by listing them as a comma separated list. Options
        /// can be negated by prefixing them with `no-`. For example `no-bell,notify`.
        ///
        /// Available since 1.3.0.
        pub notify_on_command_finish_action as "notify-on-command-finish-action": NotifyOnCommandFinishAction = NotifyOnCommandFinishAction { bell: true, notify: false },
        /// If command finished notifications are enabled, this controls how long a
        /// command must have been running before a notification will be sent. The
        /// default is five seconds.
        ///
        /// The duration is specified as a series of numbers followed by time units.
        /// Whitespace is allowed between numbers and units. Each number and unit will
        /// be added together to form the total duration.
        ///
        /// The allowed time units are as follows:
        ///
        ///   * `y` - 365 SI days, or 8760 hours, or 31536000 seconds. No adjustments
        ///     are made for leap years or leap seconds.
        ///   * `d` - one SI day, or 86400 seconds.
        ///   * `h` - one hour, or 3600 seconds.
        ///   * `m` - one minute, or 60 seconds.
        ///   * `s` - one second.
        ///   * `ms` - one millisecond, or 0.001 second.
        ///   * `us` or `µs` - one microsecond, or 0.000001 second.
        ///   * `ns` - one nanosecond, or 0.000000001 second.
        ///
        /// Examples:
        ///   * `1h30m`
        ///   * `45s`
        ///
        /// Units can be repeated and will be added together. This means that
        /// `1h1h` is equivalent to `2h`. This is confusing and should be avoided.
        /// A future update may disallow this.
        ///
        /// The maximum value is `584y 49w 23h 34m 33s 709ms 551µs 615ns`. Any
        /// value larger than this will be clamped to the maximum value.
        ///
        /// Available since 1.3.0
        pub notify_on_command_finish_after as "notify-on-command-finish-after": Duration = Duration { duration: 5 * NS_PER_S },
        /// Extra environment variables to pass to commands launched in a terminal
        /// surface. The format is `env=KEY=VALUE`.
        ///
        /// `env = foo=bar`
        /// `env = bar=baz`
        ///
        /// Setting `env` to an empty string will reset the entire map to default
        /// (empty).
        ///
        /// `env =`
        ///
        /// Setting a key to an empty string will remove that particular key and
        /// corresponding value from the map.
        ///
        /// `env = foo=bar`
        /// `env = foo=`
        ///
        /// will result in `foo` not being passed to the launched commands.
        ///
        /// Setting a key multiple times will overwrite previous entries.
        ///
        /// `env = foo=bar`
        /// `env = foo=baz`
        ///
        /// will result in `foo=baz` being passed to the launched commands.
        ///
        /// These environment variables will override any existing environment
        /// variables set by Ghostty. For example, if you set `GHOSTTY_RESOURCES_DIR`
        /// then the value you set here will override the value Ghostty typically
        /// automatically injects.
        ///
        /// These environment variables _will not_ be passed to commands run by Ghostty
        /// for other purposes, like `open` or `xdg-open` used to open URLs in your
        /// browser.
        ///
        /// Available since: 1.2.0
        pub env: RepeatableStringMap = RepeatableStringMap::default(),
        /// Data to send as input to the command on startup.
        ///
        /// The configured `command` will be launched using the typical rules,
        /// then the data specified as this input will be written to the pty
        /// before any other input can be provided.
        ///
        /// The bytes are sent as-is with no additional encoding. Therefore, be
        /// cautious about input that can contain control characters, because this
        /// can be used to execute programs in a shell.
        ///
        /// The format of this value is:
        ///
        ///   * `raw:<string>` - Send raw text as-is. This uses Zig string literal
        ///     syntax so you can specify control characters and other standard
        ///     escapes.
        ///
        ///   * `path:<path>` - Read a filepath and send the contents. The path
        ///     must be to a file with finite length. e.g. don't use a device
        ///     such as `/dev/stdin` or `/dev/urandom` as these will block
        ///     terminal startup indefinitely. Files are limited to 10MB
        ///     in size to prevent excessive memory usage. If you have files
        ///     larger than this you should write a script to read the file
        ///     and send it to the terminal.
        ///
        /// If no valid prefix is found, it is assumed to be a `raw:` input.
        /// This is an ergonomic choice to allow you to simply write
        /// `input = "Hello, world!"` (a common case) without needing to prefix
        /// every value with `raw:`.
        ///
        /// This can be repeated multiple times to send more data. The data
        /// is concatenated directly with no separator characters in between
        /// (e.g. no newline).
        ///
        /// If any of the input sources do not exist, then none of the input
        /// will be sent. Input sources are not verified until the terminal
        /// is starting, so missing paths will not show up in config validation.
        ///
        /// Changing this configuration at runtime will only affect new
        /// terminals.
        ///
        /// Available since: 1.2.0
        pub input: RepeatableReadableIO = RepeatableReadableIO::default(),
        /// If true, keep the terminal open after the command exits. Normally, the
        /// terminal window closes when the running command (such as a shell) exits.
        /// With this true, the terminal window will stay open until any keypress is
        /// received.
        ///
        /// This is primarily useful for scripts or debugging.
        pub wait_after_command as "wait-after-command": bool = false,
        /// The number of milliseconds of runtime below which we consider a process exit
        /// to be abnormal. This is used to show an error message when the process exits
        /// too quickly.
        ///
        /// On Linux, this must be paired with a non-zero exit code. On macOS, we allow
        /// any exit code because of the way shell processes are launched via the login
        /// command.
        pub abnormal_command_exit_runtime as "abnormal-command-exit-runtime": u32 = 250,
        /// The size of the scrollback buffer in bytes. This also includes the active
        /// screen. No matter what this is set to, enough memory will always be
        /// allocated for the visible screen and anything leftover is the limit for
        /// the scrollback.
        ///
        /// When this limit is reached, the oldest lines are removed from the
        /// scrollback.
        ///
        /// Scrollback is stored in memory and allocated lazily up to this limit, so
        /// setting a very large limit does not immediately consume that amount of
        /// memory. On supported systems with scrollback compression enabled, Ghostty
        /// attempts to compress fully historical pages which are not currently visible
        /// while the terminal is idle. This can reduce physical memory usage, depending
        /// on the contents of the scrollback.
        ///
        /// This limit always measures the uncompressed logical size of the terminal
        /// pages. Compression does not allow Ghostty to retain more history than the
        /// configured limit. Accessing compressed history restores it transparently
        /// and may increase the terminal's physical memory usage again.
        ///
        /// This size is per terminal surface, not for the entire application.
        ///
        /// A separate maximum can be set with `scrollback-limit-lines`; if both limits
        /// are set, then the first one reached will determine when scrollback is
        /// removed.
        ///
        /// The default is 50 MB. Set this to `unlimited` to remove the byte limit.
        ///
        /// This can be changed at runtime but will only affect new terminal surfaces.
        pub scrollback_limit_bytes as "scrollback-limit-bytes": Limit<usize, 50_000_000> = Limit::default(),
        /// The maximum number of lines of scrollback to retain. This excludes
        /// the active screen. Soft-wrapped lines count as multiple lines.
        ///
        /// This limit is an estimate. Internally, Ghostty will only trim lines
        /// up to the minimum allocation unit that is used internally (called a
        /// "page"). The size of a page depends on how many styles, graphemes, etc.
        /// take up the screen. In practice, this can be anywhere from a handful to
        /// a couple hundred lines. Importantly, memory is capped either way.
        /// This means that the actual limited lines will likely be slightly
        /// higher in practice.
        ///
        /// The default is `unlimited`. A separate maximum can be set with
        /// `scrollback-limit-bytes`; if both limits are set, then the first one reached
        /// will determine when scrollback is removed.
        ///
        /// This can be changed at runtime but will only affect new terminal surfaces.
        pub scrollback_limit_lines as "scrollback-limit-lines": Limit<usize, { usize::MAX as i128 }> = Limit::default(),
        /// Whether to compress scrollback pages while the terminal is idle.
        ///
        /// Ghostty does its best to only compress when idle and decompress
        /// as needed. This means that compression doesn't lower IO throughput.
        /// We recommend you keep it on.
        ///
        /// The scrollback limit remains an uncompressed logical limit regardless of
        /// this setting, so disabling compression can increase physical memory usage
        /// but does not change how much history is retained.
        ///
        /// Text-heavy terminal history generally compresses to approximately 10% to
        /// 30% of its uncompressed page memory, corresponding to a 70% to 90% reduction
        /// in physical memory for pages which are compressed. Compression savings are
        /// content-dependent.
        ///
        /// Note that the way Ghostty works is that we compress and discard the
        /// physical/resident memory but we retain virtual mappings. You will not
        /// see a decrease in virtual memory usage, but you will see a decrease
        /// in physical/memory usage.
        ///
        /// Changing this at runtime affects future compression work. Pages which are
        /// already compressed remain compressed until their contents are accessed.
        pub scrollback_compression as "scrollback-compression": bool = true,
        /// Control when the scrollbar is shown to scroll the scrollback buffer.
        ///
        /// The default value is `system`.
        ///
        /// Valid values:
        ///
        ///   * `system` - Respect the system settings for when to show scrollbars.
        ///     On macOS, we only show scrollbars while actively scrolling or hovering
        ///     the gutter. If the system setting is set to "Always", the scrollbar
        ///     will be shown when the mouse is over the gutter area.
        ///
        ///   * `never` - Never show a scrollbar. You can still scroll using the mouse,
        ///     keybind actions, etc. but you will not have a visual UI widget showing
        ///     a scrollbar.
        pub scrollbar: Scrollbar = Scrollbar::System,
        /// Match a regular expression against the terminal text and associate clicking
        /// it with an action. This can be used to match URLs, file paths, etc. Actions
        /// can be opening using the system opener (e.g. `open` or `xdg-open`) or
        /// executing any arbitrary binding action.
        ///
        /// Links that are configured earlier take precedence over links that are
        /// configured later.
        ///
        /// A default link that matches a URL and opens it in the system opener always
        /// exists. This can be disabled using `link-url`.
        ///
        /// TODO: This can't currently be set!
        pub link: RepeatableLink = RepeatableLink::default(),
        /// Enable URL matching. URLs are matched on hover with control (Linux) or
        /// command (macOS) pressed and open using the default system application for
        /// the linked URL.
        ///
        /// The URL matcher is always lowest priority of any configured links (see
        /// `link`). If you want to customize URL matching, use `link` and disable this.
        pub link_url as "link-url": bool = true,
        /// Enable hyperlinks created with the OSC 8 escape sequence. When disabled,
        /// OSC 8 hyperlinks are not highlighted, previewed, copied, or opened.
        ///
        /// This does not affect URL matching controlled by `link-url`.
        ///
        /// Available since: 1.4.0
        pub link_osc8 as "link-osc8": bool = true,
        /// Show link previews for a matched URL.
        ///
        /// When true, link previews are shown for all matched URLs. When false, link
        /// previews are never shown. When set to "osc8", link previews are only shown
        /// for hyperlinks created with the OSC 8 sequence (in this case, the link text
        /// can differ from the link destination).
        ///
        /// Available since: 1.2.0
        pub link_previews as "link-previews": LinkPreviews = LinkPreviews::True,
        /// Whether to start the window in a maximized state. This setting applies
        /// to new windows and does not apply to tabs, splits, etc. However, this setting
        /// will apply to all new windows, not just the first one.
        ///
        /// Available since: 1.1.0
        pub maximize: bool = false,
        /// Start new windows in fullscreen. This setting applies to new windows and
        /// does not apply to tabs, splits, etc. However, this setting will apply to all
        /// new windows, not just the first one.
        ///
        /// Allowable values are:
        ///
        ///   * `false` - Don't start in fullscreen (default)
        ///   * `true` - Start in native fullscreen
        ///   * `non-native` - (macOS only) Start in non-native fullscreen, hiding the
        ///     menu bar. This is faster than native fullscreen since it doesn't use
        ///     animations. On non-macOS platforms, this behaves the same as `true`.
        ///   * `non-native-visible-menu` - (macOS only) Start in non-native fullscreen,
        ///     keeping the menu bar visible. On non-macOS platforms, behaves like `true`.
        ///   * `non-native-padded-notch` - (macOS only) Start in non-native fullscreen,
        ///     hiding the menu bar but padding for the notch on applicable devices.
        ///     On non-macOS platforms, behaves like `true`.
        ///
        /// Important: tabs DO NOT WORK with non-native fullscreen modes. Non-native
        /// fullscreen removes the titlebar and macOS native tabs require the titlebar.
        /// If you use tabs, use `true` (native) instead.
        ///
        /// On macOS, `true` (native fullscreen) does not work if `window-decoration`
        /// is set to `false`, because native fullscreen on macOS requires window
        /// decorations.
        pub fullscreen: Fullscreen = Fullscreen::False,
        /// The title Ghostty will use for the window. This will force the title of the
        /// window to be this title at all times and Ghostty will ignore any set title
        /// escape sequences programs (such as Neovim) may send.
        ///
        /// If you want a blank title, set this to one or more spaces by quoting
        /// the value. For example, `title = " "`. This effectively hides the title.
        /// This is necessary because setting a blank value resets the title to the
        /// default value of the running program.
        ///
        /// This configuration can be reloaded at runtime. If it is set, the title
        /// will update for all windows. If it is unset, the next title change escape
        /// sequence will be honored but previous changes will not retroactively
        /// be set. This latter case may require you to restart programs such as Neovim
        /// to get the new title.
        pub title: Option<StringZ> = None,
        /// The setting that will change the application class value.
        ///
        /// This controls the class field of the `WM_CLASS` X11 property (when running
        /// under X11), the Wayland application ID (when running under Wayland), and the
        /// bus name that Ghostty uses to connect to DBus.
        ///
        /// Note that changing this value between invocations will create new, separate
        /// instances, of Ghostty when running with `gtk-single-instance=true`. See that
        /// option for more details.
        ///
        /// Changing this value may break launching Ghostty from `.desktop` files, via
        /// DBus activation, or systemd user services as the system is expecting Ghostty
        /// to connect to DBus using the default `class` when it is launched.
        ///
        /// The class name must follow the requirements defined [in the GTK
        /// documentation](https://docs.gtk.org/gio/type_func.Application.id_is_valid.html).
        ///
        /// The default is `com.mitchellh.ghostty`.
        ///
        /// This only affects GTK builds.
        pub class: Option<StringZ> = None,
        /// This controls the instance name field of the `WM_CLASS` X11 property when
        /// running under X11. It has no effect otherwise.
        ///
        /// The default is `ghostty`.
        ///
        /// This only affects GTK builds.
        pub x11_instance_name as "x11-instance-name": Option<StringZ> = None,
        /// The directory to change to after starting the command.
        ///
        /// This setting is secondary to the `window-inherit-working-directory`
        /// setting. If a previous Ghostty terminal exists in the same process,
        /// `window-inherit-working-directory` will take precedence. Otherwise, this
        /// setting will be used. Typically, this setting is used only for the first
        /// window.
        ///
        /// The default is `inherit` except in special scenarios listed next. On macOS,
        /// if Ghostty can detect it is launched from launchd (double-clicked) or
        /// `open`, then it defaults to `home`. On Linux with GTK, if Ghostty can detect
        /// it was launched from a desktop launcher, then it defaults to `home`.
        ///
        /// The value of this must be an absolute path, a path prefixed with `~/`
        /// (the tilde will be expanded to the user's home directory), or
        /// one of the special values below:
        ///
        ///   * `home` - The home directory of the executing user.
        ///
        ///   * `inherit` - The working directory of the launching process.
        pub working_directory as "working-directory": Option<WorkingDirectory> = None,
        /// Key bindings. The format is `trigger=action`. Duplicate triggers will
        /// overwrite previously set values. The list of actions is available in
        /// the documentation or using the `ghostty +list-actions` command.
        ///
        /// Trigger: `+`-separated list of keys and modifiers. Example: `ctrl+a`,
        /// `ctrl+shift+b`, `up`.
        ///
        /// If the key is a single Unicode codepoint, the trigger will match
        /// any presses that produce that codepoint. These are impacted by
        /// keyboard layouts. For example, `a` will match the `a` key on a
        /// QWERTY keyboard, but will match the `q` key on a AZERTY keyboard
        /// (assuming US physical layout).
        ///
        /// For Unicode codepoints, matching is done by comparing the set of
        /// modifiers with the unmodified codepoint. The unmodified codepoint is
        /// sometimes called an "unshifted character" in other software, but all
        /// modifiers are considered, not only shift. For example, `ctrl+a` will match
        /// `a` but not `ctrl+shift+a` (which is `A` on a US keyboard).
        ///
        /// Further, codepoint matching is case-insensitive and the unmodified
        /// codepoint is always case folded for comparison. As a result,
        /// `ctrl+A` configured will match when `ctrl+a` is pressed. Note that
        /// this means some key combinations are impossible depending on keyboard
        /// layout. For example, `ctrl+_` is impossible on a US keyboard because
        /// `_` is `shift+-` and `ctrl+shift+-` is not equal to `ctrl+_` (because
        /// the modifiers don't match!). More details on impossible key combinations
        /// can be found at this excellent source written by Qt developers:
        /// https://doc.qt.io/qt-6/qkeysequence.html#keyboard-layout-issues
        ///
        /// Physical key codes can be specified by using any of the key codes
        /// as specified by the [W3C specification](https://www.w3.org/TR/uievents-code/).
        /// For example, `KeyA` will match the physical `a` key on a US standard
        /// keyboard regardless of the keyboard layout. These are case-sensitive.
        ///
        /// For aesthetic reasons, the w3c codes also support snake case. For
        /// example, `key_a` is equivalent to `KeyA`. The only exceptions are
        /// function keys, e.g. `F1` is `f1` (no underscore). This is a consequence
        /// of our internal code using snake case but is purposely supported
        /// and tested so it is safe to use. It allows an all-lowercase binding
        /// which I find more aesthetically pleasing.
        ///
        /// Function keys such as `insert`, `up`, `f5`, etc. are also specified
        /// using the keys as specified by the previously linked W3C specification.
        ///
        /// Physical keys always match with a higher priority than Unicode codepoints,
        /// so if you specify both `a` and `KeyA`, the physical key will always be used
        /// regardless of what order they are configured.
        ///
        /// The special key `catch_all` can be used to match any key that is not
        /// otherwise bound. This can be combined with modifiers, for example
        /// `ctrl+catch_all` will match any key pressed with `ctrl` that is not
        /// otherwise bound. When looking up a binding, Ghostty first tries to match
        /// `catch_all` with modifiers. If no match is found and the event has
        /// modifiers, it falls back to `catch_all` without modifiers.
        ///
        /// Valid modifiers are `shift`, `ctrl` (alias: `control`), `alt` (alias: `opt`,
        /// `option`), and `super` (alias: `cmd`, `command`). You may use the modifier
        /// or the alias. When debugging keybinds, the non-aliased modifier will always
        /// be used in output.
        ///
        /// Note: The fn or "globe" key on keyboards are not supported as a
        /// modifier. This is a limitation of the operating systems and GUI toolkits
        /// that Ghostty uses.
        ///
        /// Some additional notes for triggers:
        ///
        ///   * modifiers cannot repeat, `ctrl+ctrl+a` is invalid.
        ///
        ///   * modifiers and keys can be in any order, `shift+a+ctrl` is *weird*,
        ///     but valid.
        ///
        ///   * only a single key input is allowed, `ctrl+a+b` is invalid.
        ///
        /// You may also specify multiple triggers separated by `>` to require a
        /// sequence of triggers to activate the action. For example,
        /// `ctrl+a>n=new_window` will only trigger the `new_window` action if the
        /// user presses `ctrl+a` followed separately by `n`. In other software, this
        /// is sometimes called a leader key, a key chord, a key table, etc. There
        /// is no hardcoded limit on the number of parts in a sequence.
        ///
        /// Warning: If you define a sequence as a CLI argument to `ghostty`,
        /// you probably have to quote the keybind since `>` is a special character
        /// in most shells. Example: ghostty --keybind='ctrl+a>n=new_window'
        ///
        /// A trigger sequence has some special handling:
        ///
        ///   * Ghostty will wait an indefinite amount of time for the next key in
        ///     the sequence. There is no way to specify a timeout. The only way to
        ///     force the output of a prefix key is to assign another keybind to
        ///     specifically output that key (e.g. `ctrl+a>ctrl+a=text:foo`) or
        ///     press an unbound key which will send both keys to the program.
        ///
        ///   * If an unbound key is pressed during a sequence and a `catch_all`
        ///     binding exists that would `ignore` the input, the entire sequence
        ///     is dropped and nothing happens. Otherwise, the entire sequence is
        ///     encoded and sent to the running program as if no keybind existed.
        ///
        ///   * If a prefix in a sequence is previously bound, the sequence will
        ///     override the previous binding. For example, if `ctrl+a` is bound to
        ///     `new_window` and `ctrl+a>n` is bound to `new_tab`, pressing `ctrl+a`
        ///     will do nothing.
        ///
        ///   * Adding to the above, if a previously bound sequence prefix is
        ///     used in a new, non-sequence binding, the entire previously bound
        ///     sequence will be unbound. For example, if you bind `ctrl+a>n` and
        ///     `ctrl+a>t`, and then bind `ctrl+a` directly, both `ctrl+a>n` and
        ///     `ctrl+a>t` will become unbound.
        ///
        ///   * Trigger sequences are not allowed for `global:` or `all:`-prefixed
        ///     triggers. This is a limitation we could remove in the future.
        ///
        /// Action is the action to take when the trigger is satisfied. It takes the
        /// format `action` or `action:param`. The latter form is only valid if the
        /// action requires a parameter.
        ///
        ///   * `ignore` - Do nothing, ignore the key input. This can be used to
        ///     black hole certain inputs to have no effect.
        ///
        ///   * `unbind` - Remove the binding. This makes it so the previous action
        ///     is removed, and the key will be sent through to the child command
        ///     if it is printable. Unbind will remove any matching trigger,
        ///     including `physical:`-prefixed triggers without specifying the
        ///     prefix.
        ///
        ///   * `csi:text` - Send a CSI sequence. e.g. `csi:A` sends "cursor up".
        ///
        ///   * `esc:text` - Send an escape sequence. e.g. `esc:d` deletes to the
        ///     end of the word to the right.
        ///
        ///   * `text:text` - Send a string. Uses Zig string literal syntax.
        ///     e.g. `text:\x15` sends Ctrl-U.
        ///
        ///   * All other actions can be found in the documentation or by using the
        ///     `ghostty +list-actions` command.
        ///
        /// Some notes for the action:
        ///
        ///   * The parameter is taken as-is after the `:`. Double quotes or
        ///     other mechanisms are included and NOT parsed. If you want to
        ///     send a string value that includes spaces, wrap the entire
        ///     trigger/action in double quotes. Example: `--keybind="up=csi:A B"`
        ///
        /// There are some additional special values that can be specified for
        /// keybind:
        ///
        ///   * `keybind=clear` will clear all set keybindings. Warning: this
        ///     removes ALL keybindings up to this point, including the default
        ///     keybindings.
        ///
        /// The keybind trigger can be prefixed with some special values to change
        /// the behavior of the keybind. These are:
        ///
        ///  * `all:`
        ///
        ///    Make the keybind apply to all terminal surfaces. By default,
        ///    keybinds only apply to the focused terminal surface. If this is true,
        ///    then the keybind will be sent to all terminal surfaces. This only
        ///    applies to actions that are surface-specific. For actions that
        ///    are already global (e.g. `quit`), this prefix has no effect.
        ///
        ///    Available since: 1.0.0
        ///
        ///  * `global:`
        ///
        ///    Make the keybind global. By default, keybinds only work within Ghostty
        ///    and under the right conditions (application focused, sometimes terminal
        ///    focused, etc.). If you want a keybind to work globally across your system
        ///    (e.g. even when Ghostty is not focused), specify this prefix.
        ///    This prefix implies `all:`.
        ///
        ///    Note: this does not work in all environments; see the additional notes
        ///    below for more information.
        ///
        ///    Available since: 1.0.0 on macOS, 1.2.0 on GTK
        ///
        ///  * `unconsumed:`
        ///
        ///    Do not consume the input. By default, a keybind will consume the input,
        ///    meaning that the associated encoding (if any) will not be sent to the
        ///    running program in the terminal. If you wish to send the encoded value
        ///    to the program, specify the `unconsumed:` prefix before the entire
        ///    keybind. For example: `unconsumed:ctrl+a=reload_config`. `global:` and
        ///    `all:`-prefixed keybinds will always consume the input regardless of
        ///    this setting. Since they are not associated with a specific terminal
        ///    surface, they're never encoded.
        ///
        ///    Available since: 1.0.0
        ///
        ///  * `performable:`
        ///
        ///    Only consume the input if the action is able to be performed.
        ///    For example, the `copy_to_clipboard` action will only consume the input
        ///    if there is a selection to copy. If there is no selection, Ghostty
        ///    behaves as if the keybind was not set. This has no effect with `global:`
        ///    or `all:`-prefixed keybinds. For key sequences, this will reset the
        ///    sequence if the action is not performable (acting identically to not
        ///    having a keybind set at all).
        ///
        ///    Performable keybinds will not appear as menu shortcuts in the
        ///    application menu. This is because the menu shortcuts force the
        ///    action to be performed regardless of the state of the terminal.
        ///    Performable keybinds will still work, they just won't appear as
        ///    a shortcut label in the menu.
        ///
        ///    Available since: 1.1.0
        ///
        /// Keybind triggers are not unique per prefix combination. For example,
        /// `ctrl+a` and `global:ctrl+a` are not two separate keybinds. The keybind
        /// set later will overwrite the keybind set earlier. In this case, the
        /// `global:` keybind will be used.
        ///
        /// Multiple prefixes can be specified. For example,
        /// `global:unconsumed:ctrl+a=reload_config` will make the keybind global
        /// and not consume the input to reload the config.
        ///
        /// On macOS, this feature requires accessibility permissions to be granted
        /// to Ghostty. When a `global:` keybind is specified and Ghostty is launched
        /// or reloaded, Ghostty will attempt to request these permissions.
        /// If the permissions are not granted, the keybind will not work. On macOS,
        /// you can find these permissions in System Preferences -> Privacy & Security
        /// -> Accessibility.
        ///
        /// Since Ghostty 1.4.0, global shortcuts on Linux are primarily supported
        /// through the [`vicinae-hotkey-v1`](https://github.com/vicinaehq/vicinae-wayland-protocols/tree/main/staging/vicinae-hotkey)
        /// protocol, available out-of-the-box with the following Wayland compositors
        /// and desktop environments:
        ///
        ///   - Hyprland since version 0.56.0
        ///
        /// On X11 or when your Wayland compositor/desktop environment does not
        /// support `vicinae-hotkey-v1`, the much more widely-used
        /// [XDG Global Shortcuts](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html)
        /// protocol is used instead.
        ///
        /// Note: XDG Global Shortcuts rely on a functional XDG Desktop Portal
        /// implementation **that is designed to be used with your desktop environment
        /// or compositor**. Please make sure that you have the correct one installed.
        ///
        /// Desktop environments that are known to support the XDG Global Shortcuts
        /// protocol include:
        ///
        ///  - KDE Plasma since [5.27](https://kde.org/announcements/plasma/5/5.27.0/#wayland)
        ///    with `xdg-desktop-portal-kde` installed
        ///
        ///  - GNOME since [48](https://release.gnome.org/48/#and-thats-not-all) with
        ///    `xdg-desktop-portal-gnome` installed
        ///
        ///  - Hyprland before version 0.56.0, with `xdg-desktop-portal-hyprland`
        ///    installed. **Some manual configuration is required.** Consult the steps
        ///    outlined on the [Hyprland Wiki](https://wiki.hypr.land/Configuring/Basics/Binds/#dbus-global-shortcuts)
        ///    to set up D-Bus-based global shortcuts correctly.
        ///
        /// Desktop environments and compositors that don't implement either protocol
        /// do not support global shortcuts, like wlroots-based compositors e.g. Sway
        /// (see [upstream issue](https://github.com/emersion/xdg-desktop-portal-wlr/issues/240))
        /// and COSMIC.
        ///
        /// ## Chained Actions
        ///
        /// A keybind can have multiple actions by using the `chain` keyword for
        /// subsequent actions. When a keybind is activated, all chained actions are
        /// executed in order. The syntax is:
        ///
        /// ```ini
        /// keybind = ctrl+a=new_window
        /// keybind = chain=goto_split:left
        /// ```
        ///
        /// This binds `ctrl+a` to first open a new window, then move focus to the
        /// left split. Each `chain` entry appends an action to the most recently
        /// defined keybind. You can chain as many actions as you want:
        ///
        /// ```ini
        /// keybind = ctrl+a=new_window
        /// keybind = chain=goto_split:left
        /// keybind = chain=toggle_fullscreen
        /// ```
        ///
        /// Chained actions cannot have prefixes like `global:` or `unconsumed:`.
        /// The flags from the original keybind apply to the entire chain.
        ///
        /// Chained actions work with key sequences as well. For example:
        ///
        /// ```ini
        /// keybind = ctrl+a>n=new_window
        /// keybind = chain=goto_split:left
        /// ````
        ///
        /// Chains with key sequences apply to the most recent binding in the
        /// sequence.
        ///
        /// Chained keybinds are available since Ghostty 1.3.0.
        ///
        /// ## Key Tables
        ///
        /// You may also create a named set of keybindings known as a "key table."
        /// A key table must be explicitly activated for the bindings to become
        /// available. This can be used to implement features such as a
        /// "copy mode", "vim mode", etc. Generically, this can implement modal
        /// keyboard input.
        ///
        /// Key tables are defined using the syntax `<table>/<binding>`. The
        /// `<binding>` value is everything documented above for keybinds. The
        /// `<table>` value is the name of the key table. Table names can contain
        /// anything except `/`, `=`, `+`, and `>`. The characters `+` and `>` are
        /// reserved for keybind syntax (modifier combinations and key sequences).
        /// For example `foo/ctrl+a=new_window` defines a binding within a table
        /// named `foo`.
        ///
        /// Tables are activated and deactivated using the binding actions
        /// `activate_key_table:<name>` and `deactivate_key_table`. Other table
        /// related binding actions also exist; see the documentation for a full list.
        /// These are the primary way to interact with key tables.
        ///
        /// Binding lookup proceeds from the innermost table outward, so keybinds in
        /// the default table remain available unless explicitly unbound in an inner
        /// table.
        ///
        /// A key table has some special syntax and handling:
        ///
        ///   * `<name>/` (with no binding) defines and clears a table, resetting all
        ///     of its keybinds and settings.
        ///
        ///   * You cannot activate a table that is already the innermost table; such
        ///     attempts are ignored. However, the same table can appear multiple times
        ///     in the stack as long as it is not innermost (e.g., `A -> B -> A -> B`
        ///     is valid, but `A -> B -> B` is not).
        ///
        ///   * A table can be activated in one-shot mode using
        ///     `activate_key_table_once:<name>`. A one-shot table is automatically
        ///     deactivated when any non-catch-all binding is invoked.
        ///
        ///   * Key sequences work within tables: `foo/ctrl+a>ctrl+b=new_window`.
        ///     If an invalid key is pressed, the sequence ends but the table remains
        ///     active.
        ///
        ///   * Chain actions work within tables, the `chain` keyword applies to
        ///     the most recently defined binding in the table. e.g. if you set
        ///     `table/ctrl+a=new_window` you can chain by using `chain=text:hello`.
        ///     Important: chain itself doesn't get prefixed with the table name,
        ///     since it applies to the most recent binding in any table.
        ///
        ///   * Prefixes like `global:` work within tables:
        ///     `foo/global:ctrl+a=new_window`.
        ///
        /// Key tables are available since Ghostty 1.3.0.
        pub keybind: Keybinds = Keybinds::default(),
        /// Remap modifier keys within Ghostty. This allows you to swap or reassign
        /// modifier keys at the application level without affecting system-wide
        /// settings.
        ///
        /// The format is `from=to` where both `from` and `to` are modifier key names.
        /// You can use generic names like `ctrl`, `alt`, `shift`, `super` (macOS:
        /// `cmd`/`command`) or sided names like `left_ctrl`, `right_alt`, etc.
        ///
        /// This will NOT change keyboard layout or key encodings outside of Ghostty.
        /// For example, on macOS, `option+a` may still produce `å` even if `option` is
        /// remapped to `ctrl`. Desktop environments usually handle key layout long
        /// before Ghostty receives the key events.
        ///
        /// Example:
        ///
        ///     key-remap = ctrl=super
        ///     key-remap = left_control=right_alt
        ///
        /// Important notes:
        ///
        /// * This is a one-way remap. If you remap `ctrl=super`, then the physical
        ///   Ctrl key acts as Super, but the Super key remains Super.
        ///
        /// * Remaps are not transitive. If you remap `ctrl=super` and `alt=ctrl`,
        ///   pressing Alt will produce Ctrl, NOT Super.
        ///
        /// * This affects both keybind matching and terminal input encoding.
        ///   This does NOT impact keyboard layout or how keys are interpreted
        ///   prior to Ghostty receiving them. For example, `option+a` on macOS
        ///   may still produce `å` even if `option` is remapped to `ctrl`.
        ///
        /// * Generic modifiers (e.g. `ctrl`) match both left and right physical keys.
        ///   Use sided names (e.g. `left_ctrl`) to remap only one side.
        ///
        /// There are other edge case scenarios that may not behave as expected
        /// but are working as intended the way this feature is designed:
        ///
        /// * On macOS, bindings in the main menu will trigger before any remapping
        ///   is done. This is because macOS itself handles menu activation and
        ///   this happens before Ghostty receives the key event. To workaround
        ///   this, you should unbind the menu items and rebind them using your
        ///   desired modifier.
        ///
        /// This configuration can be repeated to specify multiple remaps.
        pub key_remap as "key-remap": KeyRemapSet = KeyRemapSet::empty(),
        /// Horizontal window padding. This applies padding between the terminal cells
        /// and the left and right window borders. The value is in points, meaning that
        /// it will be scaled appropriately for screen DPI.
        ///
        /// If this value is set too large, the screen will render nothing, because the
        /// grid will be completely squished by the padding. It is up to you as the user
        /// to pick a reasonable value. If you pick an unreasonable value, a warning
        /// will appear in the logs.
        ///
        /// Changing this configuration at runtime will only affect new terminals, i.e.
        /// new windows, tabs, etc.
        ///
        /// To set a different left and right padding, specify two numerical values
        /// separated by a comma. For example, `window-padding-x = 2,4` will set the
        /// left padding to 2 and the right padding to 4. If you want to set both
        /// paddings to the same value, you can use a single value. For example,
        /// `window-padding-x = 2` will set both paddings to 2.
        pub window_padding_x as "window-padding-x": WindowPadding = WindowPadding { top_left: 2, bottom_right: 2 },
        /// Vertical window padding. This applies padding between the terminal cells and
        /// the top and bottom window borders. The value is in points, meaning that it
        /// will be scaled appropriately for screen DPI.
        ///
        /// If this value is set too large, the screen will render nothing, because the
        /// grid will be completely squished by the padding. It is up to you as the user
        /// to pick a reasonable value. If you pick an unreasonable value, a warning
        /// will appear in the logs.
        ///
        /// Changing this configuration at runtime will only affect new terminals,
        /// i.e. new windows, tabs, etc.
        ///
        /// To set a different top and bottom padding, specify two numerical values
        /// separated by a comma. For example, `window-padding-y = 2,4` will set the
        /// top padding to 2 and the bottom padding to 4. If you want to set both
        /// paddings to the same value, you can use a single value. For example,
        /// `window-padding-y = 2` will set both paddings to 2.
        pub window_padding_y as "window-padding-y": WindowPadding = WindowPadding { top_left: 2, bottom_right: 2 },
        /// The viewport dimensions are usually not perfectly divisible by the cell
        /// size. In this case, some extra padding on the end of a column and the bottom
        /// of the final row may exist. If this is `true`, then this extra padding
        /// is automatically balanced between all four edges to minimize imbalance on
        /// one side. If this is `false`, the top left grid cell will always hug the
        /// edge with zero padding other than what may be specified with the other
        /// `window-padding` options.
        ///
        /// If other `window-padding` fields are set and this is `true`, this will still
        /// apply. The other padding is applied first and may affect how many grid cells
        /// actually exist, and this is applied last in order to balance the padding
        /// given a certain viewport size and grid cell size.
        ///
        /// Valid values are:
        ///
        /// * `false` - No balancing is applied.
        /// * `true` - Balance the padding, but cap the top padding to avoid
        ///   excessive space above the first row. Any excess is shifted to the
        ///   bottom.
        /// * `equal` - Balance the padding equally on all sides without any
        ///   top-padding cap. (Available since: 1.4.0)
        pub window_padding_balance as "window-padding-balance": WindowPaddingBalance = WindowPaddingBalance::False,
        /// The color of the padding area of the window. Valid values are:
        ///
        /// * `background` - The background color specified in `background`.
        /// * `extend` - Extend the background color of the nearest grid cell.
        /// * `extend-always` - Same as "extend" but always extends without applying
        ///   any of the heuristics that disable extending noted below.
        ///
        /// The "extend" value will be disabled in certain scenarios. On primary
        /// screen applications (e.g. not something like Neovim), the color will not
        /// be extended vertically if any of the following are true:
        ///
        /// * The nearest row has any cells that have the default background color.
        ///   The thinking is that in this case, the default background color looks
        ///   fine as a padding color.
        /// * The nearest row is a prompt row (requires shell integration). The
        ///   thinking here is that prompts often contain powerline glyphs that
        ///   do not look good extended.
        /// * The nearest row contains a perfect fit powerline character. These
        ///   don't look good extended.
        pub window_padding_color as "window-padding-color": WindowPaddingColor = WindowPaddingColor::Background,
        /// Synchronize rendering with the screen refresh rate. If true, this will
        /// minimize tearing and align redraws with the screen but may cause input
        /// latency. If false, this will maximize redraw frequency but may cause tearing,
        /// and under heavy load may use more CPU and power.
        ///
        /// This defaults to true because out-of-sync rendering on macOS can
        /// cause kernel panics (macOS 14.4+) and performance issues for external
        /// displays over some hardware such as DisplayLink. If you want to minimize
        /// input latency, set this to false with the known aforementioned risks.
        ///
        /// Changing this value at runtime will only affect new terminals.
        ///
        /// This setting is only supported currently on macOS.
        pub window_vsync as "window-vsync": bool = true,
        /// If true, new windows will inherit the working directory of the
        /// previously focused window. If no window was previously focused, the default
        /// working directory will be used (the `working-directory` option).
        pub window_inherit_working_directory as "window-inherit-working-directory": bool = true,
        /// If true, new tabs will inherit the working directory of the
        /// previously focused tab. If no tab was previously focused, the default
        /// working directory will be used (the `working-directory` option).
        pub tab_inherit_working_directory as "tab-inherit-working-directory": bool = true,
        /// If true, new split panes will inherit the working directory of the
        /// previously focused split. If no split was previously focused, the default
        /// working directory will be used (the `working-directory` option).
        pub split_inherit_working_directory as "split-inherit-working-directory": bool = true,
        /// If true, new windows and tabs will inherit the font size of the previously
        /// focused window. If no window was previously focused, the default font size
        /// will be used. If this is false, the default font size specified in the
        /// configuration `font-size` will be used.
        pub window_inherit_font_size as "window-inherit-font-size": bool = true,
        /// Configure a preference for window decorations. This setting specifies
        /// a _preference_; the actual OS, desktop environment, window manager, etc.
        /// may override this preference. Ghostty will do its best to respect this
        /// preference but it may not always be possible.
        ///
        /// Valid values:
        ///
        ///  * `none`
        ///
        ///    All window decorations will be disabled. Titlebar, borders, etc. will
        ///    not be shown. On macOS, this will also disable tabs (enforced by the
        ///    system).
        ///
        ///  * `auto`
        ///
        ///    Automatically decide to use either client-side or server-side
        ///    decorations based on the detected preferences of the current OS and
        ///    desktop environment. This option usually makes Ghostty look the most
        ///    "native" for your desktop.
        ///
        ///  * `client`
        ///
        ///    Prefer client-side decorations.
        ///
        ///    Available since: 1.1.0
        ///
        ///  * `server`
        ///
        ///    Prefer server-side decorations. This is only relevant on Linux with GTK,
        ///    either on X11, or Wayland on a compositor that supports the
        ///    `org_kde_kwin_server_decoration` protocol (e.g. KDE Plasma, but almost
        ///    any non-GNOME desktop supports this protocol).
        ///
        ///    If `server` is set but the environment doesn't support server-side
        ///    decorations, client-side decorations will be used instead.
        ///
        ///    Available since: 1.1.0
        ///
        /// The default value is `auto`.
        ///
        /// For the sake of backwards compatibility and convenience, this setting also
        /// accepts boolean true and false values. If set to `true`, this is equivalent
        /// to `auto`. If set to `false`, this is equivalent to `none`.
        /// This is convenient for users who live primarily on systems that don't
        /// differentiate between client and server-side decorations (e.g. macOS and
        /// Windows).
        ///
        /// The "toggle_window_decorations" keybind action can be used to create
        /// a keybinding to toggle this setting at runtime.
        ///
        /// macOS: To hide the titlebar without removing the native window borders
        ///        or rounded corners, use `macos-titlebar-style = hidden` instead.
        pub window_decoration as "window-decoration": WindowDecoration = WindowDecoration::Auto,
        /// The font that will be used for the application's window and tab titles.
        ///
        /// If this setting is left unset, the system default font will be used.
        ///
        /// Note: any font available on the system may be used, this font is not
        /// required to be a fixed-width font.
        ///
        /// Available since: 1.0.0 on macOS, 1.1.0 on GTK
        pub window_title_font_family as "window-title-font-family": Option<StringZ> = None,
        /// The text that will be displayed in the subtitle of the window. Valid values:
        ///
        ///   * `false` - Disable the subtitle.
        ///   * `working-directory` - Set the subtitle to the working directory of the
        ///      surface.
        ///
        /// This feature is only supported on GTK.
        ///
        /// Available since: 1.1.0
        pub window_subtitle as "window-subtitle": WindowSubtitle = WindowSubtitle::False,
        /// The theme to use for the windows. Valid values:
        ///
        ///   * `auto` - Determine the theme based on the configured terminal
        ///      background color. This has no effect if the "theme" configuration
        ///      has separate light and dark themes. In that case, the behavior
        ///      of "auto" is equivalent to "system".
        ///   * `system` - Use the system theme.
        ///   * `light` - Use the light theme regardless of system theme.
        ///   * `dark` - Use the dark theme regardless of system theme.
        ///   * `ghostty` - Use the background and foreground colors specified in the
        ///     Ghostty configuration. This is only supported on Linux builds.
        ///
        /// On macOS, if `macos-titlebar-style` is `tabs` or `transparent`, the window theme will be
        /// automatically set based on the luminosity of the terminal background color.
        /// This only applies to terminal windows. This setting will still apply to
        /// non-terminal windows within Ghostty.
        ///
        /// This is currently only supported on macOS and Linux.
        pub window_theme as "window-theme": WindowTheme = WindowTheme::Auto,
        /// The color space to use when interpreting terminal colors. "Terminal colors"
        /// refers to colors specified in your configuration and colors produced by
        /// direct-color SGR sequences.
        ///
        /// Valid values:
        ///
        ///   * `srgb` - Interpret colors in the sRGB color space. This is the default.
        ///   * `display-p3` - Interpret colors in the Display P3 color space.
        ///
        /// This setting is currently only supported on macOS.
        pub window_colorspace as "window-colorspace": WindowColorspace = WindowColorspace::Srgb,
        /// The initial window size. This size is in terminal grid cells by default.
        /// Both values must be set to take effect. If only one value is set, it is
        /// ignored.
        ///
        /// We don't currently support specifying a size in pixels but a future change
        /// can enable that. If this isn't specified, the app runtime will determine
        /// some default size.
        ///
        /// Note that the window manager may put limits on the size or override the
        /// size. For example, a tiling window manager may force the window to be a
        /// certain size to fit within the grid. There is nothing Ghostty will do about
        /// this, but it will make an effort.
        ///
        /// Sizes larger than the screen size will be clamped to the screen size.
        /// This can be used to create a maximized-by-default window size.
        ///
        /// This will not affect new tabs, splits, or other nested terminal elements.
        /// This only affects the initial window size of any new window. Changing this
        /// value will not affect the size of the window after it has been created. This
        /// is only used for the initial size.
        ///
        /// BUG: On Linux with GTK, the calculated window size will not properly take
        /// into account window decorations. As a result, the grid dimensions will not
        /// exactly match this configuration. If window decorations are disabled (see
        /// `window-decoration`), then this will work as expected.
        ///
        /// Windows smaller than 10 wide by 4 high are not allowed.
        pub window_height as "window-height": u32 = 0,
        pub window_width as "window-width": u32 = 0,
        /// The starting window position. This position is in pixels and is relative
        /// to the top-left corner of the primary monitor. Both values must be set to take
        /// effect. If only one value is set, it is ignored.
        ///
        /// Note that the window manager may put limits on the position or override
        /// the position. For example, a tiling window manager may force the window
        /// to be a certain position to fit within the grid. There is nothing Ghostty
        /// will do about this, but it will make an effort.
        ///
        /// Also note that negative values are also up to the operating system and
        /// window manager. Some window managers may not allow windows to be placed
        /// off-screen.
        ///
        /// Invalid positions are runtime-specific, but generally the positions are
        /// clamped to the nearest valid position.
        ///
        /// On macOS, the window position is relative to the top-left corner of
        /// the visible screen area. This means that if the menu bar is visible, the
        /// window will be placed below the menu bar.
        ///
        /// Note: this is only supported on macOS. The GTK runtime does not support
        /// setting the window position, as windows are only allowed position
        /// themselves in X11 and not Wayland.
        pub window_position_x as "window-position-x": Option<i16> = None,
        pub window_position_y as "window-position-y": Option<i16> = None,
        /// Whether to enable saving and restoring window state. Window state includes
        /// their position, size, tabs, splits, etc. Some window state requires shell
        /// integration, such as preserving working directories. See `shell-integration`
        /// for more information.
        ///
        /// There are three valid values for this configuration:
        ///
        ///   * `default` will use the default system behavior. On macOS, this
        ///     will only save state if the application is forcibly terminated
        ///     or if it is configured systemwide via Settings.app.
        ///
        ///   * `never` will never save window state.
        ///
        ///   * `always` will always save window state whenever Ghostty is exited.
        ///
        /// If you change this value to `never` while Ghostty is not running, the next
        /// Ghostty launch will NOT restore the window state.
        ///
        /// If you change this value to `default` while Ghostty is not running and the
        /// previous exit saved state, the next Ghostty launch will still restore the
        /// window state. This is because Ghostty cannot know if the previous exit was
        /// due to a forced save or not (macOS doesn't provide this information).
        ///
        /// If you change this value so that window state is saved while Ghostty is not
        /// running, the previous window state will not be restored because Ghostty only
        /// saves state on exit if this is enabled.
        ///
        /// The default value is `default`.
        ///
        /// This is currently only supported on macOS. This has no effect on Linux.
        pub window_save_state as "window-save-state": WindowSaveState = WindowSaveState::Default,
        /// Resize the window in discrete increments of the focused surface's cell size.
        /// If this is disabled, surfaces are resized in pixel increments. Currently
        /// only supported on macOS.
        pub window_step_resize as "window-step-resize": bool = false,
        /// The position where new tabs are created. Valid values:
        ///
        ///   * `current` - Insert the new tab after the currently focused tab,
        ///     or at the end if there are no focused tabs.
        ///
        ///   * `end` - Insert the new tab at the end of the tab list.
        pub window_new_tab_position as "window-new-tab-position": WindowNewTabPosition = WindowNewTabPosition::Current,
        /// Whether to show the tab bar.
        ///
        /// Valid values:
        ///
        ///  - `always`
        ///
        ///    Always display the tab bar, even when there's only one tab.
        ///
        ///    Available since: 1.2.0
        ///
        ///  - `auto` *(default)*
        ///
        ///    Automatically show and hide the tab bar. The tab bar is only
        ///    shown when there are two or more tabs present.
        ///
        ///  - `never`
        ///
        ///    Never show the tab bar. Tabs are only accessible via the tab
        ///    overview or by keybind actions.
        ///
        /// Currently only supported on Linux (GTK).
        pub window_show_tab_bar as "window-show-tab-bar": WindowShowTabBar = WindowShowTabBar::Auto,
        /// Background color for the window titlebar. This only takes effect if
        /// window-theme is set to ghostty. Currently only supported in the GTK app
        /// runtime.
        ///
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        pub window_titlebar_background as "window-titlebar-background": Option<Color> = None,
        /// Foreground color for the window titlebar. This only takes effect if
        /// window-theme is set to ghostty. Currently only supported in the GTK app
        /// runtime.
        ///
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        pub window_titlebar_foreground as "window-titlebar-foreground": Option<Color> = None,
        /// Controls when drag handles are shown over splits,
        /// allowing splits to be rearranged with mouse controls.
        ///
        /// Valid values:
        ///
        ///  - `always`
        ///
        ///    Always display the drag handle, even when there's only one split.
        ///
        ///  - `auto` *(default)*
        ///
        ///    Automatically show and hide the drag handle.
        ///
        ///    On Linux, the handle is only shown when there are two or
        ///    more splits present.
        ///
        ///    On macOS, the handle is only hidden when there's one split
        ///    in **fullscreen** mode, otherwise it's shown when hovered.
        ///
        ///  - `never`
        ///
        ///    Never show the drag handle. Splits then cannot be rearranged with
        ///    mouse controls.
        ///
        /// Available since: 1.4.0.
        pub drag_handle as "drag-handle": DragHandle = DragHandle::Auto,
        /// This controls when resize overlays are shown. Resize overlays are a
        /// transient popup that shows the size of the terminal while the surfaces are
        /// being resized. The possible options are:
        ///
        ///   * `always` - Always show resize overlays.
        ///   * `never` - Never show resize overlays.
        ///   * `after-first` - The resize overlay will not appear when the surface
        ///                     is first created, but will show up if the surface is
        ///                     subsequently resized.
        ///
        /// The default is `after-first`.
        pub resize_overlay as "resize-overlay": ResizeOverlay = ResizeOverlay::AfterFirst,
        /// If resize overlays are enabled, this controls the position of the overlay.
        /// The possible options are:
        ///
        ///   * `center`
        ///   * `top-left`
        ///   * `top-center`
        ///   * `top-right`
        ///   * `bottom-left`
        ///   * `bottom-center`
        ///   * `bottom-right`
        ///
        /// The default is `center`.
        pub resize_overlay_position as "resize-overlay-position": ResizeOverlayPosition = ResizeOverlayPosition::Center,
        /// If resize overlays are enabled, this controls how long the overlay is
        /// visible on the screen before it is hidden. The default is ¾ of a second or
        /// 750 ms.
        ///
        /// The duration is specified as a series of numbers followed by time units.
        /// Whitespace is allowed between numbers and units. Each number and unit will
        /// be added together to form the total duration.
        ///
        /// The allowed time units are as follows:
        ///
        ///   * `y` - 365 SI days, or 8760 hours, or 31536000 seconds. No adjustments
        ///     are made for leap years or leap seconds.
        ///   * `d` - one SI day, or 86400 seconds.
        ///   * `h` - one hour, or 3600 seconds.
        ///   * `m` - one minute, or 60 seconds.
        ///   * `s` - one second.
        ///   * `ms` - one millisecond, or 0.001 second.
        ///   * `us` or `µs` - one microsecond, or 0.000001 second.
        ///   * `ns` - one nanosecond, or 0.000000001 second.
        ///
        /// Examples:
        ///   * `1h30m`
        ///   * `45s`
        ///
        /// Units can be repeated and will be added together. This means that
        /// `1h1h` is equivalent to `2h`. This is confusing and should be avoided.
        /// A future update may disallow this.
        ///
        /// The maximum value is `584y 49w 23h 34m 33s 709ms 551µs 615ns`. Any
        /// value larger than this will be clamped to the maximum value.
        ///
        /// Available since 1.0.0
        pub resize_overlay_duration as "resize-overlay-duration": Duration = Duration { duration: 750 * NS_PER_MS },
        /// If true, when there are multiple split panes, the mouse selects the pane
        /// that is focused. This only applies to the currently focused window; e.g.
        /// mousing over a split in an unfocused window will not focus that split
        /// and bring the window to front.
        ///
        /// Default is false.
        pub focus_follows_mouse as "focus-follows-mouse": bool = false,
        /// Whether to allow programs running in the terminal to read/write to the
        /// system clipboard (OSC 52, for googling). The default is to allow clipboard
        /// reading after prompting the user and allow writing unconditionally.
        ///
        /// Valid values are:
        ///
        ///   * `ask`
        ///   * `allow`
        ///   * `deny`
        ///
        pub clipboard_read as "clipboard-read": ClipboardAccess = ClipboardAccess::Ask,
        pub clipboard_write as "clipboard-write": ClipboardAccess = ClipboardAccess::Allow,
        /// The maximum size in bytes of a single clipboard write by a program
        /// running in the terminal via the Kitty clipboard protocol (OSC 5522).
        /// This doesn't apply to OSC 52, which is limited by the maximum length
        /// of an escape sequence hardcoded into Ghostty for now.
        ///
        /// Data beyond the limit fails the entire write with an `EFBIG` status,
        /// discards the transaction, and leaves the clipboard untouched. Later
        /// write-related packets are ignored until a new write begins.
        ///
        /// The data is buffered in memory while the write is in progress, so
        /// this limit bounds how much memory a program can make Ghostty
        /// allocate per write. A future improvement will attempt to spool large
        /// writes to disk.
        ///
        /// The default is 64 MiB, the minimum a conforming implementation must
        /// accept. Set this to `unlimited` to remove the limit, allowing writes
        /// bounded only by available memory. A value of `0` rejects every non-empty
        /// write. To reject clipboard writes entirely, use `clipboard-write = deny`
        /// instead.
        ///
        /// This can be changed at runtime and applies to writes that begin
        /// after the change.
        ///
        /// Available since: 1.4.0
        pub clipboard_write_limit_bytes as "clipboard-write-limit-bytes": Limit<usize, { 64 * 1024 * 1024 }> = Limit::default(),
        /// Trims trailing whitespace on data that is copied to the clipboard. This does
        /// not affect data sent to the clipboard via `clipboard-write`. This only
        /// applies to trailing whitespace on lines that have other characters.
        /// Completely blank lines always have their whitespace trimmed.
        pub clipboard_trim_trailing_spaces as "clipboard-trim-trailing-spaces": bool = true,
        /// Require confirmation before pasting text that appears unsafe. This helps
        /// prevent a "copy/paste attack" where a user may accidentally execute unsafe
        /// commands by pasting text with newlines or carriage returns.
        pub clipboard_paste_protection as "clipboard-paste-protection": bool = true,
        /// If true, bracketed pastes will be considered safe. By default, bracketed
        /// pastes are considered safe. "Bracketed" pastes are pastes while the running
        /// program has bracketed paste mode enabled (a setting set by the running
        /// program, not the terminal emulator).
        pub clipboard_paste_bracketed_safe as "clipboard-paste-bracketed-safe": bool = true,
        /// Enables or disabled title reporting (CSI 21 t). This escape sequence
        /// allows the running program to query the terminal title. This is a common
        /// security issue and is disabled by default.
        ///
        /// Warning: This can expose sensitive information at best and enable
        /// arbitrary code execution at worst (with a maliciously crafted title
        /// and a minor amount of user interaction).
        ///
        /// Available since: 1.0.1
        pub title_report as "title-report": bool = false,
        /// The total amount of bytes that can be used for image data (e.g. the Kitty
        /// image protocol) per terminal screen. The maximum value is 4,294,967,295
        /// (4GiB). The default is 320MB. If this is set to zero, then all image
        /// protocols will be disabled.
        ///
        /// This value is separate for primary and alternate screens so the effective
        /// limit per surface is double.
        pub image_storage_limit as "image-storage-limit": u32 = 320 * 1000 * 1000,
        /// Whether to automatically copy selected text to the clipboard.
        ///
        /// Valid values:
        ///
        /// * `none` - Do not copy selected text automatically.
        ///
        /// * `primary` - On Linux, copy to the selection clipboard only. This has no
        ///   effect on macOS. (Available since: 1.4.0)
        ///
        /// * `clipboard` - Copy text to the system clipboard only.
        ///
        /// * `both` - Copy to both clipboards on Linux, and only the system clipboard
        ///   on macOS. (Available since: 1.4.0)
        ///
        /// For backward compatibility and convenience, a value of `true` is the same as
        /// `primary` on Linux and `clipboard` on macOS, and `false` is an alias for
        /// `none`.
        ///
        /// The default value is `primary` on Linux and `none` otherwise.
        pub copy_on_select as "copy-on-select": CopyOnSelect = if cfg!(target_os = "linux") {
        CopyOnSelect::Primary
    } else {
        CopyOnSelect::None
    },
        /// The action to take when the user right-clicks on the terminal surface.
        ///
        /// Valid values:
        ///   * `context-menu` - Show the context menu.
        ///   * `paste` - Paste the contents of the clipboard.
        ///   * `copy` - Copy the selected text to the clipboard.
        ///   * `copy-or-paste` - If there is a selection, copy the selected text to
        ///      the clipboard; otherwise, paste the contents of the clipboard.
        ///   * `ignore` - Do nothing, ignore the right-click.
        ///
        /// The default value is `context-menu`.
        pub right_click_action as "right-click-action": RightClickAction = RightClickAction::ContextMenu,
        /// The action to take when the user middle-clicks on the terminal surface.
        ///
        /// Valid values:
        ///   * `primary-paste` - Paste from the selection clipboard on Linux.
        ///      Does nothing on macOS.
        ///   * `clipboard-paste` - Paste from the system clipboard.
        ///   * `ignore` - Do nothing, ignore the middle click.
        ///
        /// The default value is `primary-paste`.
        pub middle_click_action as "middle-click-action": MiddleClickAction = MiddleClickAction::PrimaryPaste,
        /// The time in milliseconds between clicks to consider a click a repeat
        /// (double, triple, etc.) or an entirely new single click. A value of zero will
        /// use a platform-specific default. The default on macOS is determined by the
        /// OS settings. On every other platform it is 500ms.
        pub click_repeat_interval as "click-repeat-interval": u32 = 0,
        /// Additional configuration files to read. This configuration can be repeated
        /// to read multiple configuration files. Configuration files themselves can
        /// load more configuration files. Paths are relative to the file containing the
        /// `config-file` directive. For command-line arguments, paths are relative to
        /// the current working directory.
        ///
        /// Prepend a ? character to the file path to suppress errors if the file does
        /// not exist. If you want to include a file that begins with a literal ?
        /// character, surround the file path in double quotes (").
        ///
        /// Cycles are not allowed. If a cycle is detected, an error will be logged and
        /// the configuration file will be ignored.
        ///
        /// Configuration files are loaded after the configuration they're defined
        /// within in the order they're defined. **THIS IS A VERY SUBTLE BUT IMPORTANT
        /// POINT.** To put it another way: configuration files do not take effect
        /// until after the entire configuration is loaded. For example, in the
        /// configuration below:
        ///
        /// ```
        /// config-file = "foo"
        /// a = 1
        /// ```
        ///
        /// If "foo" contains `a = 2`, the final value of `a` will be 2, because
        /// `foo` is loaded after the configuration file that configures the
        /// nested `config-file` value.
        pub config_file as "config-file": RepeatablePath = RepeatablePath::default(),
        /// When this is true, the default configuration file paths will be loaded.
        /// The default configuration file paths are currently only the XDG
        /// config path ($XDG_CONFIG_HOME/ghostty/config.ghostty).
        ///
        /// If this is false, the default configuration paths will not be loaded.
        /// This is targeted directly at using Ghostty from the CLI in a way
        /// that minimizes external effects.
        ///
        /// This is a CLI-only configuration. Setting this in a configuration file
        /// will have no effect. It is not an error, but it will not do anything.
        /// This configuration can only be set via CLI arguments.
        pub config_default_files as "config-default-files": bool = true,
        /// Confirms that a surface should be closed before closing it.
        ///
        /// This defaults to `true`. If set to `false`, surfaces will close without
        /// any confirmation. This can also be set to `always`, which will always
        /// confirm closing a surface, even if shell integration says a process isn't
        /// running.
        pub confirm_close_surface as "confirm-close-surface": ConfirmCloseSurface = ConfirmCloseSurface::True,
        /// Whether or not to quit after the last surface is closed.
        ///
        /// This defaults to `false` on macOS since that is standard behavior for
        /// a macOS application. On Linux, this defaults to `true` since that is
        /// generally expected behavior.
        ///
        /// On Linux, if this is `true`, Ghostty can delay quitting fully until a
        /// configurable amount of time has passed after the last window is closed.
        /// See the documentation of `quit-after-last-window-closed-delay`.
        pub quit_after_last_window_closed as "quit-after-last-window-closed": bool = cfg!(target_os = "linux"),
        /// Controls how long Ghostty will stay running after the last open surface has
        /// been closed. This only has an effect if `quit-after-last-window-closed` is
        /// also set to `true`.
        ///
        /// The minimum value for this configuration is `1s`. Any values lower than
        /// this will be clamped to `1s`.
        ///
        /// The duration is specified as a series of numbers followed by time units.
        /// Whitespace is allowed between numbers and units. Each number and unit will
        /// be added together to form the total duration.
        ///
        /// The allowed time units are as follows:
        ///
        ///   * `y` - 365 SI days, or 8760 hours, or 31536000 seconds. No adjustments
        ///     are made for leap years or leap seconds.
        ///   * `d` - one SI day, or 86400 seconds.
        ///   * `h` - one hour, or 3600 seconds.
        ///   * `m` - one minute, or 60 seconds.
        ///   * `s` - one second.
        ///   * `ms` - one millisecond, or 0.001 second.
        ///   * `us` or `µs` - one microsecond, or 0.000001 second.
        ///   * `ns` - one nanosecond, or 0.000000001 second.
        ///
        /// Examples:
        ///   * `1h30m`
        ///   * `45s`
        ///
        /// Units can be repeated and will be added together. This means that
        /// `1h1h` is equivalent to `2h`. This is confusing and should be avoided.
        /// A future update may disallow this.
        ///
        /// The maximum value is `584y 49w 23h 34m 33s 709ms 551µs 615ns`. Any
        /// value larger than this will be clamped to the maximum value.
        ///
        /// By default `quit-after-last-window-closed-delay` is unset and
        /// Ghostty will quit immediately after the last window is closed if
        /// `quit-after-last-window-closed` is `true`.
        ///
        /// Only implemented on Linux.
        pub quit_after_last_window_closed_delay as "quit-after-last-window-closed-delay": Option<Duration> = None,
        /// This controls whether an initial window is created when Ghostty
        /// is run. Note that if `quit-after-last-window-closed` is `true` and
        /// `quit-after-last-window-closed-delay` is set, setting `initial-window` to
        /// `false` will mean that Ghostty will quit after the configured delay if no
        /// window is ever created. Only implemented on Linux and macOS.
        pub initial_window as "initial-window": bool = true,
        /// The duration that undo operations remain available. After this
        /// time, the operation will be removed from the undo stack and
        /// cannot be undone.
        ///
        /// The default value is 5 seconds.
        ///
        /// This timeout applies per operation, meaning that if you perform
        /// multiple operations, each operation will have its own timeout.
        /// New operations do not reset the timeout of previous operations.
        ///
        /// A timeout of zero will effectively disable undo operations. It is
        /// not possible to set an infinite timeout, but you can set a very
        /// large timeout to effectively disable the timeout (on the order of years).
        /// This is highly discouraged, as it will cause the undo stack to grow
        /// indefinitely, memory usage to grow unbounded, and terminal sessions
        /// to never actually quit.
        ///
        /// The duration is specified as a series of numbers followed by time units.
        /// Whitespace is allowed between numbers and units. Each number and unit will
        /// be added together to form the total duration.
        ///
        /// The allowed time units are as follows:
        ///
        ///   * `y` - 365 SI days, or 8760 hours, or 31536000 seconds. No adjustments
        ///     are made for leap years or leap seconds.
        ///   * `d` - one SI day, or 86400 seconds.
        ///   * `h` - one hour, or 3600 seconds.
        ///   * `m` - one minute, or 60 seconds.
        ///   * `s` - one second.
        ///   * `ms` - one millisecond, or 0.001 second.
        ///   * `us` or `µs` - one microsecond, or 0.000001 second.
        ///   * `ns` - one nanosecond, or 0.000000001 second.
        ///
        /// Examples:
        ///   * `1h30m`
        ///   * `45s`
        ///
        /// Units can be repeated and will be added together. This means that
        /// `1h1h` is equivalent to `2h`. This is confusing and should be avoided.
        /// A future update may disallow this.
        ///
        /// This configuration is only supported on macOS. Linux doesn't
        /// support undo operations at all so this configuration has no
        /// effect.
        ///
        /// Available since: 1.2.0
        pub undo_timeout as "undo-timeout": Duration = Duration { duration: 5 * NS_PER_S },
        /// The position of the "quick" terminal window. To learn more about the
        /// quick terminal, see the documentation for the `toggle_quick_terminal`
        /// binding action.
        ///
        /// Valid values are:
        ///
        ///   * `top` - Terminal appears at the top of the screen.
        ///   * `bottom` - Terminal appears at the bottom of the screen.
        ///   * `left` - Terminal appears at the left of the screen.
        ///   * `right` - Terminal appears at the right of the screen.
        ///   * `center` - Terminal appears at the center of the screen.
        ///
        /// On macOS, changing this configuration requires restarting Ghostty
        /// completely.
        ///
        /// Note: There is no default keybind for toggling the quick terminal.
        /// To enable this feature, bind the `toggle_quick_terminal` action to a key.
        pub quick_terminal_position as "quick-terminal-position": QuickTerminalPosition = QuickTerminalPosition::Top,
        /// The size of the quick terminal.
        ///
        /// The size can be specified either as a percentage of the screen dimensions
        /// (height/width), or as an absolute size in pixels. Percentage values are
        /// suffixed with `%` (e.g. `20%`) while pixel values are suffixed with `px`
        /// (e.g. `300px`). A bare value without a suffix is a config error.
        ///
        /// When only one size is specified, the size parameter affects the size of
        /// the quick terminal on its *primary axis*, which depends on its position:
        /// height for quick terminals placed on the top or bottom, and width for left
        /// or right. The primary axis of a centered quick terminal depends on the
        /// monitor's orientation: height when on a landscape monitor, and width when
        /// on a portrait monitor.
        ///
        /// The *secondary axis* would be maximized for non-center positioned
        /// quick terminals unless another size parameter is specified, separated
        /// from the first by a comma (`,`). Percentage and pixel sizes can be mixed
        /// together: for instance, a size of `50%,500px` for a top-positioned quick
        /// terminal would be half a screen tall, and 500 pixels wide.
        ///
        /// Available since: 1.2.0
        pub quick_terminal_size as "quick-terminal-size": QuickTerminalSize = QuickTerminalSize::default(),
        /// The layer of the quick terminal window. The higher the layer,
        /// the more windows the quick terminal may conceal.
        ///
        /// Valid values are:
        ///
        ///  * `overlay`
        ///
        ///    The quick terminal appears in front of all windows.
        ///
        ///  * `top` (default)
        ///
        ///    The quick terminal appears in front of normal windows but behind
        ///    fullscreen overlays like lock screens.
        ///
        ///  * `bottom`
        ///
        ///    The quick terminal appears behind normal windows but in front of
        ///    wallpapers and other windows in the background layer.
        ///
        ///  * `background`
        ///
        ///    The quick terminal appears behind all windows.
        ///
        /// GTK Wayland only.
        ///
        /// Available since: 1.2.0
        pub gtk_quick_terminal_layer as "gtk-quick-terminal-layer": QuickTerminalLayer = QuickTerminalLayer::Top,
        /// The namespace for the quick terminal window.
        ///
        /// This is an identifier that is used by the Wayland compositor and/or
        /// scripts to determine the type of layer surfaces and to possibly apply
        /// unique effects.
        ///
        /// GTK Wayland only.
        ///
        /// Available since: 1.2.0
        pub gtk_quick_terminal_namespace as "gtk-quick-terminal-namespace": StringZ = StringZ::from("ghostty-quick-terminal"),
        /// The screen where the quick terminal should show up.
        ///
        /// Valid values are:
        ///
        ///  * `main` - The screen that the operating system recommends as the main
        ///    screen. On macOS, this is the screen that is currently receiving
        ///    keyboard input. This screen is defined by the operating system and
        ///    not chosen by Ghostty.
        ///
        ///  * `mouse` - The screen that the mouse is currently hovered over.
        ///
        ///  * `macos-menu-bar` - The screen that contains the macOS menu bar as
        ///    set in the display settings on macOS. This is a bit confusing because
        ///    every screen on macOS has a menu bar, but this is the screen that
        ///    contains the primary menu bar.
        ///
        /// The default value is `main` because this is the recommended screen
        /// by the operating system.
        ///
        /// On macOS, `macos-menu-bar` uses the screen containing the menu bar.
        /// On Linux/Wayland, `macos-menu-bar` is treated as equivalent to `main`.
        ///
        /// Note: On Linux, there is no universal concept of a "primary" monitor.
        /// Ghostty uses the compositor-reported primary output when available and
        /// falls back to the first monitor reported by GDK if no primary output can
        /// be resolved.
        pub quick_terminal_screen as "quick-terminal-screen": QuickTerminalScreen = QuickTerminalScreen::Main,
        /// Duration (in seconds) of the quick terminal enter and exit animation.
        /// Set it to 0 to disable animation completely. This can be changed at
        /// runtime.
        ///
        /// Only implemented on macOS.
        pub quick_terminal_animation_duration as "quick-terminal-animation-duration": f64 = 0.2,
        /// Automatically hide the quick terminal when focus shifts to another window.
        /// Set it to false for the quick terminal to remain open even when it loses focus.
        ///
        /// Defaults to true on macOS and on false on Linux/BSD. This is because global
        /// shortcuts on Linux require system configuration and are considerably less
        /// accessible than on macOS, meaning that it is more preferable to keep the
        /// quick terminal open until the user has completed their task.
        /// This default may change in the future.
        pub quick_terminal_autohide as "quick-terminal-autohide": bool = cfg!(target_os = "macos"),
        /// This configuration option determines the behavior of the quick terminal
        /// when switching between macOS spaces. macOS spaces are virtual desktops
        /// that can be manually created or are automatically created when an
        /// application is in full-screen mode.
        ///
        /// Valid values are:
        ///
        ///  * `move` - When switching to another space, the quick terminal will
        ///    also moved to the current space.
        ///
        ///  * `remain` - The quick terminal will stay only in the space where it
        ///    was originally opened and will not follow when switching to another
        ///    space.
        ///
        /// The default value is `move`.
        ///
        /// Only implemented on macOS.
        /// On Linux the behavior is always equivalent to `move`.
        ///
        /// Available since: 1.1.0
        pub quick_terminal_space_behavior as "quick-terminal-space-behavior": QuickTerminalSpaceBehavior = QuickTerminalSpaceBehavior::Move,
        /// Determines under which circumstances that the quick terminal should receive
        /// keyboard input. See the corresponding [Wayland documentation](https://wayland.app/protocols/wlr-layer-shell-unstable-v1#zwlr_layer_surface_v1:enum:keyboard_interactivity)
        /// for a more detailed explanation of the behavior of each option.
        ///
        /// > [!NOTE]
        /// > The exact behavior of each option may differ significantly across
        /// > compositors -- experiment with them on your system to find one that
        /// > suits your liking!
        ///
        /// Valid values are:
        ///
        ///  * `none`
        ///
        ///    The quick terminal will not receive any keyboard input.
        ///
        ///  * `on-demand` (default)
        ///
        ///    The quick terminal would only receive keyboard input when it is focused.
        ///
        ///  * `exclusive`
        ///
        ///    The quick terminal will always receive keyboard input, even when another
        ///    window is currently focused.
        ///
        /// Only has an effect on Linux Wayland.
        /// On macOS the behavior is always equivalent to `on-demand`.
        ///
        /// Available since: 1.2.0
        pub quick_terminal_keyboard_interactivity as "quick-terminal-keyboard-interactivity": QuickTerminalKeyboardInteractivity = QuickTerminalKeyboardInteractivity::OnDemand,
        /// Whether to enable shell integration auto-injection or not. Shell integration
        /// greatly enhances the terminal experience by enabling a number of features:
        ///
        ///   * Working directory reporting so new tabs, splits inherit the
        ///     previous terminal's working directory.
        ///
        ///   * Prompt marking that enables the "jump_to_prompt" keybinding.
        ///
        ///   * If you're sitting at a prompt, closing a terminal will not ask
        ///     for confirmation.
        ///
        ///   * Resizing the window with a complex prompt usually paints much
        ///     better.
        ///
        /// Allowable values are:
        ///
        ///   * `none` - Do not do any automatic injection. You can still manually
        ///     configure your shell to enable the integration.
        ///
        ///   * `detect` - Detect the shell based on the filename.
        ///
        ///   * `bash`, `elvish`, `fish`, `nushell`, `zsh` - Use this specific shell injection scheme.
        ///
        /// The default value is `detect`.
        pub shell_integration as "shell-integration": ShellIntegration = ShellIntegration::Detect,
        /// Shell integration features to enable. These require our shell integration
        /// to be loaded, either automatically via shell-integration or manually.
        ///
        /// The format of this is a list of features to enable separated by commas. If
        /// you prefix a feature with `no-` then it is disabled. If you omit a feature,
        /// its default value is used, so you must explicitly disable features you don't
        /// want. You can also use `true` or `false` to turn all features on or off.
        ///
        /// Example: `cursor`, `no-cursor`, `sudo`, `no-sudo`, `title`, `no-title`
        ///
        /// Available features:
        ///
        ///   * `cursor` - Set the cursor to a bar at the prompt.
        ///
        ///   * `sudo` - Set sudo wrapper to preserve terminfo.
        ///
        ///   * `title` - Set the window title via shell integration.
        ///
        ///   * `ssh-env` - Enable SSH environment variable compatibility. Automatically
        ///     converts TERM from `xterm-ghostty` to `xterm-256color` when connecting to
        ///     remote hosts and propagates COLORTERM, TERM_PROGRAM, and TERM_PROGRAM_VERSION.
        ///     Whether or not these variables will be accepted by the remote host(s) will
        ///     depend on whether or not the variables are allowed in their sshd_config.
        ///     (Available since: 1.2.0)
        ///
        ///   * `ssh-terminfo` - Enable automatic terminfo installation on remote hosts.
        ///     Attempts to install Ghostty's embedded terminfo entry using `tic` on local
        ///     cache misses. Requires `tic` to be available on remote hosts. Successful
        ///     installations are cached locally to avoid repeat installations.
        ///     If desired, the `+ssh-cache` CLI action can be used to manage the installation
        ///     cache manually using various arguments.
        ///     (Available since: 1.2.0)
        ///
        ///   * `path` - Add Ghostty's binary directory to PATH. This ensures the `ghostty`
        ///     command is available in the shell even if shell init scripts reset PATH.
        ///     This is particularly useful on macOS where PATH is often overridden by
        ///     system scripts. The directory is only added if not already present.
        ///
        /// SSH features work independently and can be combined for optimal experience:
        /// when both `ssh-env` and `ssh-terminfo` are enabled, Ghostty will install its
        /// terminfo on remote hosts and use `xterm-ghostty` as TERM, falling back to
        /// `xterm-256color` with environment variables if terminfo installation fails.
        pub shell_integration_features as "shell-integration-features": ShellIntegrationFeatures = ShellIntegrationFeatures::default(),
        /// Custom entries into the command palette.
        ///
        /// Each entry requires the title, the corresponding action, and an optional
        /// description. Each field should be prefixed with the field name, a colon
        /// (`:`), and then the specified value. The syntax for actions is identical
        /// to the one for keybind actions. Whitespace in between fields is ignored.
        ///
        /// If you need to embed commas or any other special characters in the values,
        /// enclose the value in double quotes and it will be interpreted as a Zig
        /// string literal. This is also useful for including whitespace at the
        /// beginning or the end of a value. See the
        /// [Zig documentation](https://ziglang.org/documentation/master/#Escape-Sequences)
        /// for more information on string literals. Note that multiline string literals
        /// are not supported.
        ///
        /// Double quotes can not be used around the field names.
        ///
        /// ```ini
        /// command-palette-entry = title:Reset Font Style, action:csi:0m
        /// command-palette-entry = title:Crash on Main Thread,description:Causes a crash on the main (UI) thread.,action:crash:main
        /// command-palette-entry = title:Focus Split: Right,description:"Focus the split to the right, if it exists.",action:goto_split:right
        /// command-palette-entry = title:"Ghostty",description:"Add a little Ghostty to your terminal.",action:"text:\xf0\x9f\x91\xbb"
        /// ```
        ///
        /// There are some additional special values that can be specified for
        /// command-palette-entry:
        ///
        ///   * `command-palette-entry=clear` will clear all command entries. Warning: this
        ///     removes ALL entries up to this point, including the default
        ///     entries. Available since: 1.4.0
        ///
        /// By default, the command palette is preloaded with most actions that might
        /// be useful in an interactive setting yet do not have easily accessible or
        /// memorizable shortcuts. The default entries can be restored by setting this
        /// setting to an empty value:
        ///
        /// ```ini
        /// command-palette-entry =
        /// ```
        ///
        /// Available since: 1.2.0
        pub command_palette_entry as "command-palette-entry": RepeatableCommand = RepeatableCommand::default(),
        /// Sets the reporting format for OSC sequences that request color information.
        /// Ghostty currently supports OSC 10 (foreground), OSC 11 (background), and
        /// OSC 4 (256 color palette) queries, and by default the reported values
        /// are scaled-up RGB values, where each component are 16 bits. This is how
        /// most terminals report these values. However, some legacy applications may
        /// require 8-bit, unscaled, components. We also support turning off reporting
        /// altogether. The components are lowercase hex values.
        ///
        /// Allowable values are:
        ///
        ///   * `none` - OSC 4/10/11 queries receive no reply
        ///
        ///   * `8-bit` - Color components are return unscaled, e.g. `rr/gg/bb`
        ///
        ///   * `16-bit` - Color components are returned scaled, e.g. `rrrr/gggg/bbbb`
        ///
        /// The default value is `16-bit`.
        pub osc_color_report_format as "osc-color-report-format": OSCColorReportFormat = OSCColorReportFormat::Bit16,
        /// If true, allows the "KAM" mode (ANSI mode 2) to be used within
        /// the terminal. KAM disables keyboard input at the request of the
        /// application. This is not a common feature and is not recommended
        /// to be enabled. This will not be documented further because
        /// if you know you need KAM, you know. If you don't know if you
        /// need KAM, you don't need it.
        pub vt_kam_allowed as "vt-kam-allowed": bool = false,
        /// If true, allows the running program to resize the window using the
        /// xterm `CSI 8 ; rows ; columns t` escape sequence. If a parameter is zero
        /// or omitted, the current size of that dimension is kept. Sizes smaller
        /// than 40 columns by 10 rows are raised to that size so that a program
        /// can't shrink the window to hide its output.
        ///
        /// The request is ignored if the terminal is in a split, in a window with
        /// multiple tabs, or the quick terminal, or if the window manager controls
        /// the window size, such as when it is fullscreen, maximized, or tiled.
        /// Sizes larger than the screen are clamped to the screen.
        ///
        /// This is disabled by default because it lets any program, including one
        /// running on a remote machine, change the size of your window.
        ///
        /// Available since: 1.4.0
        pub vt_window_resize_allowed as "vt-window-resize-allowed": bool = false,
        /// Enables or disables checksum reporting (DECRQCRA, `CSI Pi ; Pg ; Pt ;
        /// Pl ; Pb ; Pr * y`). This escape sequence allows the running program to
        /// ask for a checksum of an area of the screen. Terminal test suites use it
        /// to check what is on the screen.
        ///
        /// This is disabled by default because a program can ask for the checksum of
        /// one cell at a time and so read back everything on the screen, including
        /// the output of other programs.
        ///
        /// While this is disabled, XTCHECKSUM (`CSI Ps # y`), which changes how the
        /// checksum is calculated, is ignored as well.
        ///
        /// Available since: 1.4.0
        pub vt_xt_checksum_report as "vt-xt-checksum-report": bool = false,
        /// How the checksum reported for DECRQCRA (see `vt-xt-checksum-report`) is
        /// calculated. This is the calculation used after a reset, like xterm's
        /// `checksumExtension` resource. A running program can change it with
        /// XTCHECKSUM (`CSI Ps # y`) until the next reset. The default is the
        /// calculation of a real DEC terminal.
        ///
        /// Valid values are:
        ///
        ///   * `negate` - Report the negated sum, as DEC terminals do.
        ///
        ///   * `attributes` - Add bold, underline, blink, inverse, invisible, and
        ///     protected attributes to the value of each cell.
        ///
        ///   * `trim` - Omit plain spaces other than the first cell of the area.
        ///
        ///   * `undrawn` - Count cells that were never written to as spaces
        ///     instead of skipping them.
        ///
        ///   * `full` - Use the full codepoint of each cell instead of the DEC
        ///     8-bit value.
        ///
        /// Prefix a value with `no-` to disable it. For example, esctest expects
        /// `no-negate,no-attributes,no-trim,full` when run with `--xterm-checksum`,
        /// which is xterm's `checksumExtension: 23`.
        ///
        /// The bits of xterm's resource are described in its
        /// [man page](https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/xterm.man#L2870-L2896)
        /// and its [control sequence documentation](https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/ctlseqs.ms#L2562-L2569),
        /// but where those disagree with xterm's
        /// [implementation](https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/screen.c#L3162-L3290),
        /// Ghostty follows the implementation. The documentation describes bit 3
        /// as omitting cells that were never written to, but xterm skips those
        /// cells by default and counts them as spaces with the bit set, which is
        /// `undrawn` here. It also lists a bit 5 that xterm doesn't implement.
        ///
        /// Available since: 1.4.0
        pub vt_xt_checksum_extension as "vt-xt-checksum-extension": XtChecksumExtension = XtChecksumExtension::default(),
        /// Custom shaders to run after the default shaders. This is a file path
        /// to a GLSL-syntax shader for all platforms.
        ///
        /// Warning: Invalid shaders can cause Ghostty to become unusable such as by
        /// causing the window to be completely black. If this happens, you can
        /// unset this configuration to disable the shader.
        ///
        /// Custom shader support is based on and compatible with the Shadertoy shaders.
        /// Shaders should specify a `mainImage` function and the available uniforms
        /// largely match Shadertoy, with some caveats and Ghostty-specific extensions.
        ///
        /// The uniform values available to shaders are as follows:
        ///
        ///  * `sampler2D iChannel0` - Input texture.
        ///
        ///    A texture containing the current terminal screen. If multiple custom
        ///    shaders are specified, the output of previous shaders is written to
        ///    this texture, to allow combining multiple effects.
        ///
        ///  * `vec3 iResolution` - Output texture size, `[width, height, 1]` (in px).
        ///
        ///  * `float iTime` - Time in seconds since first frame was rendered.
        ///
        ///  * `float iTimeDelta` - Time in seconds since previous frame was rendered.
        ///
        ///  * `float iFrameRate` - Average framerate. (NOT CURRENTLY SUPPORTED)
        ///
        ///  * `int iFrame` - Number of frames that have been rendered so far.
        ///
        ///  * `float iChannelTime[4]` - Current time for video or sound input. (N/A)
        ///
        ///  * `vec3 iChannelResolution[4]` - Resolutions of the 4 input samplers.
        ///
        ///    Currently only `iChannel0` exists, and `iChannelResolution[0]` is
        ///    identical to `iResolution`.
        ///
        ///  * `vec4 iMouse` - Mouse input info. (NOT CURRENTLY SUPPORTED)
        ///
        ///  * `vec4 iDate` - Date/time info. (NOT CURRENTLY SUPPORTED)
        ///
        ///  * `float iSampleRate` - Sample rate for audio. (N/A)
        ///
        /// Ghostty-specific extensions:
        ///
        ///  * `vec4 iCurrentCursor` - Info about the terminal cursor.
        ///
        ///    - `iCurrentCursor.xy` is the -X, +Y corner of the current cursor.
        ///    - `iCurrentCursor.zw` is the width and height of the current cursor.
        ///
        ///  * `vec4 iPreviousCursor` - Info about the previous terminal cursor.
        ///
        ///  * `vec4 iCurrentCursorColor` - Color of the terminal cursor.
        ///
        ///  * `vec4 iPreviousCursorColor` - Color of the previous terminal cursor.
        ///
        ///  * `vec4 iCurrentCursorStyle` - Style of the terminal cursor
        ///
        ///    Macros simplified use are defined for the various cursor styles:
        ///
        ///    - `CURSORSTYLE_BLOCK` or `0`
        ///    - `CURSORSTYLE_BLOCK_HOLLOW` or `1`
        ///    - `CURSORSTYLE_BAR` or `2`
        ///    - `CURSORSTYLE_UNDERLINE` or `3`
        ///    - `CURSORSTYLE_LOCK` or `4`
        ///
        ///  * `vec4 iPreviousCursorStyle` - Style of the previous terminal cursor
        ///
        ///  * `vec4 iCursorVisible` - Visibility of the terminal cursor.
        ///
        ///  * `float iTimeCursorChange` - Timestamp of terminal cursor change.
        ///
        ///    When the terminal cursor changes position or color, this is set to
        ///    the same time as the `iTime` uniform, allowing you to compute the
        ///    time since the change by subtracting this from `iTime`.
        ///
        ///  * `float iTimeFocus` - Timestamp when the surface last gained iFocus.
        ///
        ///    When the surface gains focus, this is set to the current value of
        ///    `iTime`, similar to how `iTimeCursorChange` works. This allows you
        ///    to compute the time since focus was gained or lost by calculating
        ///    `iTime - iTimeFocus`. Use this to create animations that restart
        ///    when the terminal regains focus.
        ///
        ///  * `int iFocus` - Current focus state of the surface.
        ///
        ///    Set to 1.0 when the surface is focused, 0.0 when unfocused. This
        ///    allows shaders to detect unfocused state and avoid animation artifacts
        ///    from large time deltas caused by infrequent "deceptive frames"
        ///    (e.g., modifier key presses, link hover events in unfocused split panes).
        ///    Check `iFocus > 0` to determine if the surface is currently focused.
        ///
        ///  * `vec3 iPalette[256]` - The 256-color terminal palette.
        ///
        ///    RGB values for all 256 colors in the terminal palette, normalized
        ///    to [0.0, 1.0]. Index 0-15 are the ANSI colors, 16-231 are the 6x6x6
        ///    color cube, and 232-255 are the grayscale colors.
        ///
        ///  * `vec3 iBackgroundColor` - Terminal background color (RGB).
        ///
        ///  * `vec3 iForegroundColor` - Terminal foreground color (RGB).
        ///
        ///  * `vec3 iCursorColor` - Terminal cursor color (RGB).
        ///
        ///  * `vec3 iCursorText` - Terminal cursor text color (RGB).
        ///
        ///  * `vec3 iSelectionBackgroundColor` - Selection background color (RGB).
        ///
        ///  * `vec3 iSelectionForegroundColor` - Selection foreground color (RGB).
        ///
        /// If the shader fails to compile, the shader will be ignored. Any errors
        /// related to shader compilation will not show up as configuration errors
        /// and only show up in the log, since shader compilation happens after
        /// configuration loading on the dedicated render thread.  For interactive
        /// development, use [shadertoy.com](https://shadertoy.com).
        ///
        /// This can be repeated multiple times to load multiple shaders. The shaders
        /// will be run in the order they are specified.
        ///
        /// This can be changed at runtime and will affect all open terminals.
        pub custom_shader as "custom-shader": RepeatablePath = RepeatablePath::default(),
        /// If `true` (default), the focused terminal surface will run an animation
        /// loop when custom shaders are used. This uses slightly more CPU (generally
        /// less than 10%) but allows the shader to animate. This only runs if there
        /// are custom shaders and the terminal is focused.
        ///
        /// If this is set to `false`, the terminal and custom shader will only render
        /// when the terminal is updated. This is more efficient but the shader will
        /// not animate.
        ///
        /// This can also be set to `always`, which will always run the animation
        /// loop regardless of whether the terminal is focused or not. The animation
        /// loop will still only run when custom shaders are used. Note that this
        /// will use more CPU per terminal surface and can become quite expensive
        /// depending on the shader and your terminal usage.
        ///
        /// This can be changed at runtime and will affect all open terminals.
        pub custom_shader_animation as "custom-shader-animation": CustomShaderAnimation = CustomShaderAnimation::True,
        /// Bell features to enable if bell support is available in your runtime. Not
        /// all features are available on all runtimes. The format of this is a list of
        /// features to enable separated by commas. If you prefix a feature with `no-`
        /// then it is disabled. If you omit a feature, its default value is used.
        ///
        /// Valid values are:
        ///
        ///  * `system`
        ///
        ///    Instruct the system to notify the user using built-in system functions.
        ///    This could result in an audiovisual effect, a notification, or something
        ///    else entirely. Changing these effects require altering system settings:
        ///    for instance under the "Sound > Alert Sound" setting in GNOME,
        ///    or the "Accessibility > System Bell" settings in KDE Plasma.
        ///
        ///    On macOS, this plays the system alert sound.
        ///
        ///  * `audio`
        ///
        ///    Play a custom sound. (Available since 1.3.0 on macOS)
        ///
        ///  * `attention` *(enabled by default)*
        ///
        ///    Request the user's attention when Ghostty is unfocused, until it has
        ///    received focus again. On macOS, this will bounce the app icon in the
        ///    dock once. On Linux, the behavior depends on the desktop environment
        ///    and/or the window manager/compositor:
        ///
        ///    - On KDE, the background of the desktop icon in the task bar would be
        ///      highlighted;
        ///
        ///    - On GNOME, you may receive a notification that, when clicked, would
        ///      bring the Ghostty window into focus;
        ///
        ///    - On Sway, the window may be decorated with a distinctly colored border;
        ///
        ///    - On other systems this may have no effect at all.
        ///
        ///  * `title` *(enabled by default)*
        ///
        ///    Prepend a bell emoji (🔔) to the title of the alerted surface until the
        ///    terminal is re-focused or interacted with (such as on keyboard input).
        ///
        ///  * `border`
        ///
        ///    Display a border around the alerted surface until the terminal is
        ///    re-focused or interacted with (such as on keyboard input).
        ///
        ///    Available since: 1.2.0 on GTK, 1.2.1 on macOS
        ///
        /// Example: `audio`, `no-audio`, `system`, `no-system`
        ///
        /// Available since: 1.2.0
        pub bell_features as "bell-features": BellFeatures = BellFeatures::default(),
        /// If `audio` is an enabled bell feature, this is a path to an audio file. If
        /// the path is not absolute, it is considered relative to the directory of the
        /// configuration file that it is referenced from, or from the current working
        /// directory if this is used as a CLI flag. The path may be prefixed with `~/`
        /// to reference the user's home directory.
        ///
        /// Available since: 1.2.0 on GTK, 1.3.0 on macOS.
        pub bell_audio_path as "bell-audio-path": Option<Path> = None,
        /// If `audio` is an enabled bell feature, this is the volume to play the audio
        /// file at (relative to the system volume). This is a floating point number
        /// ranging from 0.0 (silence) to 1.0 (as loud as possible). The default is 0.5.
        ///
        /// Available since: 1.2.0 on GTK, 1.3.0 on macOS.
        pub bell_audio_volume as "bell-audio-volume": f64 = 0.5,
        /// Control the in-app notifications that Ghostty shows.
        ///
        /// On Linux (GTK), in-app notifications show up as toasts. Toasts appear
        /// overlaid on top of the terminal window. They are used to show information
        /// that is not critical but may be important.
        ///
        /// Possible notifications are:
        ///
        ///   - `clipboard-copy` (default: true) - Show a notification when text is copied
        ///     to the clipboard.
        ///   - `config-reload` (default: true) - Show a notification when
        ///     the configuration is reloaded.
        ///
        /// To specify a notification to enable, specify the name of the notification.
        /// To specify a notification to disable, prefix the name with `no-`. For
        /// example, to disable `clipboard-copy`, set this configuration to
        /// `no-clipboard-copy`. To enable it, set this configuration to `clipboard-copy`.
        ///
        /// Multiple notifications can be enabled or disabled by separating them
        /// with a comma.
        ///
        /// A value of "false" will disable all notifications. A value of "true" will
        /// enable all notifications.
        ///
        /// This configuration only applies to GTK.
        ///
        /// Available since: 1.1.0
        pub app_notifications as "app-notifications": AppNotifications = AppNotifications::default(),
        /// If anything other than false, fullscreen mode on macOS will not use the
        /// native fullscreen, but make the window fullscreen without animations and
        /// using a new space. It's faster than the native fullscreen mode since it
        /// doesn't use animations.
        ///
        /// Important: tabs DO NOT WORK in this mode. Non-native fullscreen removes
        /// the titlebar and macOS native tabs require the titlebar. If you use tabs,
        /// you should not use this mode.
        ///
        /// If you fullscreen a window with tabs, the currently focused tab will
        /// become fullscreen while the others will remain in a separate window in
        /// the background. You can switch to that window using normal window-switching
        /// keybindings such as command+tilde. When you exit fullscreen, the window
        /// will return to the tabbed state it was in before.
        ///
        /// Allowable values are:
        ///
        ///   * `true` - Use non-native macOS fullscreen, hide the menu bar
        ///   * `false` - Use native macOS fullscreen
        ///   * `visible-menu` - Use non-native macOS fullscreen, keep the menu bar
        ///     visible
        ///   * `padded-notch` - Use non-native macOS fullscreen, hide the menu bar,
        ///     but ensure the window is not obscured by the notch on applicable
        ///     devices. The area around the notch will remain transparent currently,
        ///     but in the future we may fill it with the window background color.
        ///
        /// Changing this option at runtime works, but will only apply to the next
        /// time the window is made fullscreen. If a window is already fullscreen,
        /// it will retain the previous setting until fullscreen is exited.
        pub macos_non_native_fullscreen as "macos-non-native-fullscreen": NonNativeFullscreen = NonNativeFullscreen::False,
        /// Whether the window buttons in the macOS titlebar are visible. The window
        /// buttons are the colored buttons in the upper left corner of most macOS apps,
        /// also known as the traffic lights, that allow you to close, miniaturize, and
        /// zoom the window.
        ///
        /// This setting has no effect when `window-decoration = none` or
        /// `macos-titlebar-style = hidden`, as the window buttons are always hidden in
        /// these modes.
        ///
        /// Valid values are:
        ///
        ///   * `visible` - Show the window buttons.
        ///   * `hidden` - Hide the window buttons.
        ///
        /// The default value is `visible`.
        ///
        /// Changing this option at runtime only applies to new windows.
        ///
        /// Available since: 1.2.0
        pub macos_window_buttons as "macos-window-buttons": MacWindowButtons = MacWindowButtons::Visible,
        /// The style of the macOS titlebar. Available values are: "native",
        /// "transparent", "tabs", and "hidden".
        ///
        /// The "native" style uses the native macOS titlebar with zero customization.
        /// The titlebar will match your window theme (see `window-theme`).
        ///
        /// The "transparent" style is the same as "native" but the titlebar will
        /// be transparent and allow your window background color to come through.
        /// This makes a more seamless window appearance but looks a little less
        /// typical for a macOS application and may not work well with all themes.
        ///
        /// The "transparent" style will also update in real-time to dynamic
        /// changes to the window background color, e.g. via OSC 11. To make this
        /// more aesthetically pleasing, this only happens if the terminal is
        /// a window, tab, or split that borders the top of the window. This
        /// avoids a disjointed appearance where the titlebar color changes
        /// but all the topmost terminals don't match.
        ///
        /// The "tabs" style is a completely custom titlebar that integrates the
        /// tab bar into the titlebar. This titlebar always matches the background
        /// color of the terminal. There are some limitations to this style:
        /// On macOS 13 and below, saved window state will not restore tabs correctly.
        /// macOS 14 does not have this issue and any other macOS version has not
        /// been tested.
        ///
        /// The "hidden" style hides the titlebar. Unlike `window-decoration = none`,
        /// however, it does not remove the frame from the window or cause it to have
        /// squared corners. Changing to or from this option at run-time may affect
        /// existing windows in buggy ways.
        ///
        /// When "hidden", the top titlebar area can no longer be used for dragging
        /// the window. To drag the window, you can use option+click on the resizable
        /// areas of the frame to drag the window. This is a standard macOS behavior
        /// and not something Ghostty enables.
        ///
        /// The default value is "transparent". This is an opinionated choice
        /// but its one I think is the most aesthetically pleasing and works in
        /// most cases.
        ///
        /// Changing this option at runtime only applies to new windows.
        pub macos_titlebar_style as "macos-titlebar-style": MacTitlebarStyle = MacTitlebarStyle::Transparent,
        /// Whether the proxy icon in the macOS titlebar is visible. The proxy icon
        /// is the icon that represents the folder of the current working directory.
        /// You can see this very clearly in the macOS built-in Terminal.app
        /// titlebar.
        ///
        /// The proxy icon is only visible with the native macOS titlebar style.
        ///
        /// Valid values are:
        ///
        ///   * `visible` - Show the proxy icon.
        ///   * `hidden` - Hide the proxy icon.
        ///
        /// The default value is `visible`.
        ///
        /// This setting can be changed at runtime and will affect all currently
        /// open windows but only after their working directory changes again.
        /// Therefore, to make this work after changing the setting, you must
        /// usually `cd` to a different directory, open a different file in an
        /// editor, etc.
        pub macos_titlebar_proxy_icon as "macos-titlebar-proxy-icon": MacTitlebarProxyIcon = MacTitlebarProxyIcon::Visible,
        /// Controls the windowing behavior when dropping a file or folder
        /// onto the Ghostty icon in the macOS dock.
        ///
        /// Valid values are:
        ///
        ///   * `new-tab` - Create a new tab in the current window, or open
        ///     a new window if none exist.
        ///   * `new-window` - Create a new window unconditionally.
        ///
        /// The default value is `new-tab`.
        ///
        /// This setting is only supported on macOS and has no effect on other
        /// platforms.
        pub macos_dock_drop_behavior as "macos-dock-drop-behavior": MacOSDockDropBehavior = MacOSDockDropBehavior::NewTab,
        /// macOS doesn't have a distinct "alt" key and instead has the "option"
        /// key which behaves slightly differently. On macOS by default, the
        /// option key plus a character will sometimes produce a Unicode character.
        /// For example, on US standard layouts option-b produces "∫". This may be
        /// undesirable if you want to use "option" as an "alt" key for keybindings
        /// in terminal programs or shells.
        ///
        /// This configuration lets you change the behavior so that option is treated
        /// as alt.
        ///
        /// The default behavior (unset) will depend on your active keyboard
        /// layout. If your keyboard layout is one of the keyboard layouts listed
        /// below, then the default value is "true". Otherwise, the default
        /// value is "false". Keyboard layouts with a default value of "true" are:
        ///
        ///   - U.S. Standard
        ///   - U.S. International
        ///
        /// Note that if an *Option*-sequence doesn't produce a printable character, it
        /// will be treated as *Alt* regardless of this setting. (e.g. `alt+ctrl+a`).
        ///
        /// Explicit values that can be set:
        ///
        /// If `true`, the *Option* key will be treated as *Alt*. This makes terminal
        /// sequences expecting *Alt* to work properly, but will break Unicode input
        /// sequences on macOS if you use them via the *Alt* key.
        ///
        /// You may set this to `false` to restore the macOS *Alt* key unicode
        /// sequences but this will break terminal sequences expecting *Alt* to work.
        ///
        /// The values `left` or `right` enable this for the left or right *Option*
        /// key, respectively.
        pub macos_option_as_alt as "macos-option-as-alt": Option<OptionAsAlt> = None,
        /// Whether to enable the macOS window shadow. The default value is true.
        /// With some window managers and window transparency settings, you may
        /// find false more visually appealing.
        pub macos_window_shadow as "macos-window-shadow": bool = true,
        /// If true, the macOS icon in the dock and app switcher will be hidden. This is
        /// mainly intended for those primarily using the quick-terminal mode.
        ///
        /// Note that setting this to true means that keyboard layout changes
        /// will no longer be automatic.
        ///
        /// Control whether macOS app is excluded from the dock and app switcher,
        /// a "hidden" state. This is mainly intended for those primarily using
        /// quick-terminal mode, but is a general configuration for any use
        /// case.
        ///
        /// Available values:
        ///
        ///   * `never` - The macOS app is never hidden.
        ///   * `always` - The macOS app is always hidden.
        ///
        /// Note: When the macOS application is hidden, keyboard layout changes
        /// will no longer be automatic. This is a limitation of macOS.
        ///
        /// Available since: 1.2.0
        pub macos_hidden as "macos-hidden": MacHidden = MacHidden::Never,
        /// If true, Ghostty on macOS will automatically enable the "Secure Input"
        /// feature when it detects that a password prompt is being displayed.
        ///
        /// "Secure Input" is a macOS security feature that prevents applications from
        /// reading keyboard events. This can always be enabled manually using the
        /// `Ghostty > Secure Keyboard Entry` menu item.
        ///
        /// Note that automatic password prompt detection is based on heuristics
        /// and may not always work as expected. Specifically, it does not work
        /// over SSH connections, but there may be other cases where it also
        /// doesn't work.
        ///
        /// A reason to disable this feature is if you find that it is interfering
        /// with legitimate accessibility software (or software that uses the
        /// accessibility APIs), since secure input prevents any application from
        /// reading keyboard events.
        pub macos_auto_secure_input as "macos-auto-secure-input": bool = true,
        /// If true, Ghostty will show a graphical indication when secure input is
        /// enabled. This indication is generally recommended to know when secure input
        /// is enabled.
        ///
        /// Normally, secure input is only active when a password prompt is displayed
        /// or it is manually (and typically temporarily) enabled. However, if you
        /// always have secure input enabled, the indication can be distracting and
        /// you may want to disable it.
        pub macos_secure_input_indication as "macos-secure-input-indication": bool = true,
        /// If true, Ghostty exposes and handles the built-in AppleScript dictionary
        /// on macOS.
        ///
        /// If false, all AppleScript interactions are disabled. This includes
        /// AppleScript commands and AppleScript object lookup for windows, tabs,
        /// and terminals.
        ///
        /// The default is true.
        pub macos_applescript as "macos-applescript": bool = true,
        /// Customize the macOS app icon.
        ///
        /// This only affects the icon that appears in the dock, application
        /// switcher, etc. This does not affect the icon in Finder because
        /// that is controlled by a hardcoded value in the signed application
        /// bundle and can't be changed at runtime. For more details on what
        /// exactly is affected, see the `NSApplication.icon` Apple documentation;
        /// that is the API that is being used to set the icon.
        ///
        /// Valid values:
        ///
        ///  * `official` - Use the official Ghostty icon.
        ///  * `blueprint`, `chalkboard`, `microchip`, `glass`, `holographic`,
        ///    `paper`, `retro`, `xray` - Official variants of the Ghostty icon
        ///    hand-created by artists (no AI).
        ///  * `custom` - Use a completely custom icon. The location must be specified
        ///    using the additional `macos-custom-icon` configuration
        ///  * `custom-style` - Use the official Ghostty icon but with custom
        ///    styles applied to various layers. The custom styles must be
        ///    specified using the additional `macos-icon`-prefixed configurations.
        ///    The `macos-icon-ghost-color` and `macos-icon-screen-color`
        ///    configurations are required for this style.
        ///
        /// WARNING: The `custom-style` option is _experimental_. We may change
        /// the format of the custom styles in the future. We're still finalizing
        /// the exact layers and customization options that will be available.
        ///
        /// Other caveats:
        ///
        ///   * The icon in the update dialog will always be the official icon.
        ///     This is because the update dialog is managed through a
        ///     separate framework and cannot be customized without significant
        ///     effort.
        pub macos_icon as "macos-icon": MacAppIcon = MacAppIcon::Official,
        /// The absolute path to the custom icon file.
        /// Supported formats include PNG, JPEG, and ICNS.
        ///
        /// Defaults to `~/.config/ghostty/Ghostty.icns`
        pub macos_custom_icon as "macos-custom-icon": Option<StringZ> = None,
        /// The material to use for the frame of the macOS app icon.
        ///
        /// Valid values:
        ///
        ///  * `aluminum` - A brushed aluminum frame. This is the default.
        ///  * `beige` - A classic 90's computer beige frame.
        ///  * `plastic` - A glossy, dark plastic frame.
        ///  * `chrome` - A shiny chrome frame.
        ///
        /// Note: This configuration is required when `macos-icon` is set to
        /// `custom-style`.
        pub macos_icon_frame as "macos-icon-frame": MacAppIconFrame = MacAppIconFrame::Aluminum,
        /// The color of the ghost in the macOS app icon.
        ///
        /// Note: This configuration is required when `macos-icon` is set to
        /// `custom-style`.
        ///
        /// Specified as either hex (`#RRGGBB` or `RRGGBB`) or a named X11 color.
        pub macos_icon_ghost_color as "macos-icon-ghost-color": Option<Color> = None,
        /// The color of the screen in the macOS app icon.
        ///
        /// The screen is a linear gradient so you can specify multiple colors
        /// that make up the gradient. Up to 64 comma-separated colors may be
        /// specified as either hex (`#RRGGBB` or `RRGGBB`) or as named X11
        /// colors. The first color is the bottom of the gradient and the last
        /// color is the top of the gradient.
        ///
        /// Note: This configuration is required when `macos-icon` is set to
        /// `custom-style`.
        pub macos_icon_screen_color as "macos-icon-screen-color": Option<ColorList> = None,
        /// Whether macOS Shortcuts are allowed to control Ghostty.
        ///
        /// Ghostty exposes a number of actions that allow Shortcuts to
        /// control and interact with Ghostty. This includes creating new
        /// terminals, sending text to terminals, running commands, invoking
        /// any keybind action, etc.
        ///
        /// This is a powerful feature but can be a security risk if a malicious
        /// shortcut is able to be installed and executed. Therefore, this
        /// configuration allows you to disable this feature.
        ///
        /// Valid values are:
        ///
        /// * `ask` - Ask the user whether for permission. Ghostty will remember
        ///   this choice and never ask again. This is similar to other macOS
        ///   permissions such as microphone access, camera access, etc.
        ///
        /// * `allow` - Allow Shortcuts to control Ghostty without asking.
        ///
        /// * `deny` - Deny Shortcuts from controlling Ghostty.
        ///
        /// Available since: 1.2.0
        pub macos_shortcuts as "macos-shortcuts": MacShortcuts = MacShortcuts::Ask,
        /// Put every surface (tab, split, window) into a transient `systemd` scope.
        ///
        /// This allows per-surface resource management. For example, if a shell program
        /// is using too much memory, only that shell will be killed by the oom monitor
        /// instead of the entire Ghostty process. Similarly, if a shell program is
        /// using too much CPU, only that surface will be CPU-throttled.
        ///
        /// This will cause startup times to be slower (a hundred milliseconds or so),
        /// so the default value is "single-instance." In single-instance mode, only
        /// one instance of Ghostty is running (see gtk-single-instance) so the startup
        /// time is a one-time cost. Additionally, single instance Ghostty is much
        /// more likely to have many windows, tabs, etc. so cgroup isolation is a
        /// big benefit.
        ///
        /// This feature requires `systemd`. If `systemd` is unavailable, cgroup
        /// initialization will fail. By default, this will not prevent Ghostty from
        /// working (see `linux-cgroup-hard-fail`).
        ///
        /// Changing this value and reloading the config will not affect existing
        /// surfaces.
        ///
        /// Valid values are:
        ///
        ///   * `never` - Never use cgroups.
        ///   * `always` - Always use cgroups.
        ///   * `single-instance` - Enable cgroups only for Ghostty instances launched
        ///     as single-instance applications (see gtk-single-instance).
        pub linux_cgroup as "linux-cgroup": LinuxCgroup = if cfg!(target_os = "linux") {
        LinuxCgroup::SingleInstance
    } else {
        LinuxCgroup::Never
    },
        /// Memory limit for any individual terminal process (tab, split, window,
        /// etc.) in bytes. If this is unset then no memory limit will be set.
        ///
        /// Note that this sets the `MemoryHigh` setting on the transient `systemd`
        /// scope, which is a soft limit. You should configure something like
        /// `systemd-oom` to handle killing processes that have too much memory
        /// pressure.
        ///
        /// Changing this value and reloading the config will not affect existing
        /// surfaces.
        ///
        /// See the `systemd.resource-control` manual page for more information:
        /// https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html
        pub linux_cgroup_memory_limit as "linux-cgroup-memory-limit": Option<u64> = None,
        /// Number of processes limit for any individual terminal process (tab, split,
        /// window, etc.). If this is unset then no limit will be set.
        ///
        /// Note that this sets the `TasksMax` setting on the transient `systemd` scope,
        /// which is a hard limit.
        ///
        /// Changing this value and reloading the config will not affect existing
        /// surfaces.
        ///
        /// See the `systemd.resource-control` manual page for more information:
        /// https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html
        pub linux_cgroup_processes_limit as "linux-cgroup-processes-limit": Option<u64> = None,
        /// If this is false, then creating a transient `systemd` scope (for
        /// `linux-cgroup`) will be allowed to fail and the failure is ignored. This is
        /// useful if you view cgroup isolation as a "nice to have" and not a critical
        /// resource management feature, because surface creation will not fail if
        /// `systemd` APIs fail.
        ///
        /// If this is true, then any transient `systemd` scope creation failure will
        /// cause surface creation to fail.
        ///
        /// Changing this value and reloading the config will not affect existing
        /// surfaces.
        pub linux_cgroup_hard_fail as "linux-cgroup-hard-fail": bool = false,
        /// Enable or disable GTK's OpenGL debugging logs. The default is `true` for
        /// debug builds, `false` for all others.
        ///
        /// Available since: 1.1.0
        pub gtk_opengl_debug as "gtk-opengl-debug": bool = cfg!(debug_assertions),
        /// If `true`, the Ghostty GTK application will run in single-instance mode:
        /// each new `ghostty` process launched will result in a new window if there is
        /// already a running process.
        ///
        /// If `false`, each new ghostty process will launch a separate application.
        ///
        /// If `detect`, Ghostty will assume true (single instance) unless one of
        /// the following scenarios is found:
        ///
        /// 1. TERM_PROGRAM environment variable is a non-empty value. In this
        /// case, we assume Ghostty is being launched from a graphical terminal
        /// session and you want a dedicated instance.
        ///
        /// 2. Any CLI arguments exist. In this case, we assume you are passing
        /// custom Ghostty configuration. Single instance mode inherits the
        /// configuration from when it was launched, so we must disable single
        /// instance to load the new configuration.
        ///
        /// If either of these scenarios is producing a false positive, you can
        /// set this configuration explicitly to the behavior you want.
        ///
        /// The pre-1.2 option `desktop` has been deprecated. Please replace
        /// this with `detect`.
        ///
        /// The default value is `detect`.
        ///
        /// Note that debug builds of Ghostty have a separate single-instance ID
        /// so you can test single instance without conflicting with release builds.
        pub gtk_single_instance as "gtk-single-instance": GtkSingleInstance = GtkSingleInstance::DEFAULT,
        /// When enabled, the full GTK titlebar is displayed instead of your window
        /// manager's simple titlebar. The behavior of this option will vary with your
        /// window manager.
        ///
        /// This option does nothing when `window-decoration` is none or when running
        /// under macOS.
        pub gtk_titlebar as "gtk-titlebar": bool = true,
        /// Determines the side of the screen that the GTK tab bar will stick to.
        /// Top, bottom, and hidden are supported. The default is top.
        ///
        /// When `hidden` is set, a tab button displaying the number of tabs will appear
        /// in the title bar. It has the ability to open a tab overview for displaying
        /// tabs. Alternatively, you can use the `toggle_tab_overview` action in a
        /// keybind if your window doesn't have a title bar, or you can switch tabs
        /// with keybinds.
        pub gtk_tabs_location as "gtk-tabs-location": GtkTabsLocation = GtkTabsLocation::Top,
        /// If true, the close button of each tab is shown at the start of the tab
        /// (the left, in left-to-right languages) and the tab's icon at the end.
        /// This applies to both the tab bar and the tab overview. GTK only.
        ///
        /// Available since: 1.4.0
        pub gtk_tabs_inverted as "gtk-tabs-inverted": bool = false,
        /// If this is `true`, the titlebar will be hidden when the window is maximized,
        /// and shown when the titlebar is unmaximized. GTK only.
        ///
        /// Available since: 1.1.0
        pub gtk_titlebar_hide_when_maximized as "gtk-titlebar-hide-when-maximized": bool = false,
        /// Determines the appearance of the top and bottom bars tab bar.
        ///
        /// Valid values are:
        ///
        ///  * `flat` - Top and bottom bars are flat with the terminal window.
        ///  * `raised` - Top and bottom bars cast a shadow on the terminal area.
        ///  * `raised-border` - Similar to `raised` but the shadow is replaced with a
        ///    more subtle border.
        pub gtk_toolbar_style as "gtk-toolbar-style": GtkToolbarStyle = GtkToolbarStyle::Raised,
        /// The style of the GTK titlebar. Available values are `native` and `tabs`.
        ///
        /// The `native` titlebar style is a traditional titlebar with a title, a few
        /// buttons and window controls. A separate tab bar will show up below the
        /// titlebar if you have multiple tabs open in the window.
        ///
        /// The `tabs` titlebar merges the tab bar and the traditional titlebar.
        /// This frees up vertical space on your screen if you use multiple tabs. One
        /// limitation of the `tabs` titlebar is that you cannot drag the titlebar
        /// by the titles any longer (as they are tab titles now). Other areas of the
        /// `tabs` title bar can be used to drag the window around.
        ///
        /// The default style is `native`.
        pub gtk_titlebar_style as "gtk-titlebar-style": GtkTitlebarStyle = GtkTitlebarStyle::Native,
        /// If `true` (default), then the Ghostty GTK tabs will be "wide." Wide tabs
        /// are the new typical Gnome style where tabs fill their available space.
        /// If you set this to `false` then tabs will only take up space they need,
        /// which is the old style.
        pub gtk_wide_tabs as "gtk-wide-tabs": bool = true,
        /// If `true` (default), then two-finger horizontal scrolling on a touchpad
        /// will switch between tabs. Scrolling left goes to the next tab and
        /// scrolling right goes to the previous tab. Set this to `false` to
        /// disable this behavior.
        ///
        /// Available since 1.4.0.
        pub gtk_horizontal_tab_scroll as "gtk-horizontal-tab-scroll": bool = true,
        /// Custom CSS files to be loaded.
        ///
        /// GTK CSS documentation can be found at the following links:
        ///
        ///   * https://docs.gtk.org/gtk4/css-overview.html - An overview of GTK CSS.
        ///   * https://docs.gtk.org/gtk4/css-properties.html - A comprehensive list
        ///     of supported CSS properties.
        ///
        /// Launch Ghostty with `env GTK_DEBUG=interactive ghostty` to tweak Ghostty's
        /// CSS in real time using the GTK Inspector. Errors in your CSS files would
        /// also be reported in the terminal you started Ghostty from. See
        /// https://developer.gnome.org/documentation/tools/inspector.html for more
        /// information about the GTK Inspector.
        ///
        /// This configuration can be repeated multiple times to load multiple files.
        /// Prepend a ? character to the file path to suppress errors if the file does
        /// not exist. If you want to include a file that begins with a literal ?
        /// character, surround the file path in double quotes (").
        /// The file size limit for a single stylesheet is 5MiB.
        ///
        /// Available since: 1.1.0
        pub gtk_custom_css as "gtk-custom-css": RepeatablePath = RepeatablePath::default(),
        /// If `true` (default), applications running in the terminal can show desktop
        /// notifications using certain escape sequences such as OSC 9 or OSC 777.
        pub desktop_notifications as "desktop-notifications": bool = true,
        /// If `true` (default), applications running in the terminal can show
        /// graphical progress bars using the ConEmu OSC 9;4 escape sequence.
        /// If `false`, progress bar sequences are silently ignored.
        pub progress_style as "progress-style": bool = true,
        /// Modifies the color used for bold text in the terminal.
        ///
        /// This can be set to a specific color, using the same format as
        /// `background` or `foreground` (e.g. `#RRGGBB` but other formats
        /// are also supported; see the aforementioned documentation). If a
        /// specific color is set, this color will always be used for the default
        /// bold text color. It will set the rest of the bold colors to `bright`.
        ///
        /// This can also be set to `bright`, which uses the bright color palette
        /// for bold text. For example, if the text is red, then the bold will
        /// use the bright red color. The terminal palette is set with `palette`
        /// but can also be overridden by the terminal application itself using
        /// escape sequences such as OSC 4. (Since Ghostty 1.2.0, the previous
        /// configuration `bold-is-bright` is deprecated and replaced by this
        /// usage).
        ///
        /// Available since Ghostty 1.2.0.
        pub bold_color as "bold-color": Option<BoldColor> = None,
        /// The opacity level (opposite of transparency) of the faint text. A value of
        /// 1 is fully opaque and a value of 0 is fully transparent. A value less than 0
        /// or greater than 1 will be clamped to the nearest valid value.
        ///
        /// Available since Ghostty 1.2.0.
        pub faint_opacity as "faint-opacity": f64 = 0.5,
        /// This will be used to set the `TERM` environment variable.
        /// HACK: We set this with an `xterm` prefix because vim uses that to enable key
        /// protocols (specifically this will enable `modifyOtherKeys`), among other
        /// features. An option exists in vim to modify this: `:set
        /// keyprotocol=ghostty:kitty`, however a bug in the implementation prevents it
        /// from working properly. https://github.com/vim/vim/pull/13211 fixes this.
        pub term: String = String::from("xterm-ghostty"),
        /// String to send when we receive `ENQ` (`0x05`) from the command that we are
        /// running. Defaults to an empty string if not set.
        pub enquiry_response as "enquiry-response": String = String::new(),
        /// Configures the low-level API to use for async IO, eventing, etc.
        ///
        /// Most users should leave this set to `auto`. This will automatically detect
        /// scenarios where APIs may not be available (for example `io_uring` on
        /// certain hardened kernels) and fall back to a different API. However, if
        /// you want to force a specific backend for any reason, you can set this
        /// here.
        ///
        /// Based on various benchmarks, we haven't found a statistically significant
        /// difference between the backends with regards to memory, CPU, or latency.
        /// The choice of backend is more about compatibility and features.
        ///
        /// Available options:
        ///
        ///   * `auto` - Automatically choose the best backend for the platform
        ///     based on available options.
        ///   * `epoll` - Use the `epoll` API
        ///   * `io_uring` - Use the `io_uring` API
        ///
        /// If the selected backend is not available on the platform, Ghostty will
        /// fall back to an automatically chosen backend that is available.
        ///
        /// Changing this value requires a full application restart to take effect.
        ///
        /// This is only supported on Linux, since this is the only platform
        /// where we have multiple options. On macOS, we always use `kqueue`.
        ///
        /// Available since: 1.2.0
        pub async_backend as "async-backend": AsyncBackend = AsyncBackend::Auto,
        /// Control the auto-update functionality of Ghostty. This is only supported
        /// on macOS currently, since Linux builds are distributed via package
        /// managers that are not centrally controlled by Ghostty.
        ///
        /// Checking or downloading an update does not send any information to
        /// the project beyond standard network information mandated by the
        /// underlying protocols. To put it another way: Ghostty doesn't explicitly
        /// add any tracking to the update process. The update process works by
        /// downloading information about the latest version and comparing it
        /// client-side to the current version.
        ///
        /// Valid values are:
        ///
        ///  * `off` - Disable auto-updates.
        ///  * `check` - Check for updates and notify the user if an update is
        ///    available, but do not automatically download or install the update.
        ///  * `download` - Check for updates, automatically download the update,
        ///    notify the user, but do not automatically install the update.
        ///
        /// If unset, we defer to Sparkle's default behavior, which respects the
        /// preference stored in the standard user defaults (`defaults(1)`).
        ///
        /// Changing this value at runtime works after a small delay.
        pub auto_update as "auto-update": Option<AutoUpdate> = None,
        /// The release channel to use for auto-updates.
        ///
        /// The default value of this matches the release channel of the currently
        /// running Ghostty version. If you download a pre-release version of Ghostty
        /// then this will be set to `tip` and you will receive pre-release updates.
        /// If you download a stable version of Ghostty then this will be set to
        /// `stable` and you will receive stable updates.
        ///
        /// Valid values are:
        ///
        ///  * `stable` - Stable, tagged releases such as "1.0.0".
        ///  * `tip` - Pre-release versions generated from each commit to the
        ///    main branch. This is the version that was in use during private
        ///    beta testing by thousands of people. It is generally stable but
        ///    will likely have more bugs than the stable channel.
        ///
        /// Changing this configuration requires a full restart of
        /// Ghostty to take effect.
        ///
        /// This only works on macOS since only macOS has an auto-update feature.
        pub auto_update_channel as "auto-update-channel": Option<ReleaseChannel> = None,
    }
    private {
        // Zig: `_arena: ?ArenaAllocator = null` ("This is set by the CLI
        // parser for deinit."). Dropped: every value owns its memory.

        /// List of diagnostics that were generated during the loading of
        /// the configuration.
        pub _diagnostics: DiagnosticList = DiagnosticList::default(),

        /// The conditional truths for the configuration. This is used to
        /// determine if a conditional configuration matches or not.
        pub _conditional_state: conditional::State = conditional::State::default(),

        /// The conditional keys that are used at any point during the configuration
        /// loading. This is used to speed up the conditional evaluation process.
        pub _conditional_set: ConditionalKeySet = ConditionalKeySet::default(),

        /// The steps we can use to reload the configuration after it has been loaded
        /// without reopening the files. This is used in very specific cases such
        /// as loadTheme which has more details on why.
        pub _replay_steps: Vec<ReplayStep> = Vec::new(),

        /// Set to true if Ghostty was executed as xdg-terminal-exec on Linux.
        pub _xdg_terminal_exec: bool = false,
    }
    with { format, changed, custom_hooks }
}

/// Errors that can occur while loading or finalizing a configuration
/// (the union of Zig's inferred error sets).
#[derive(Debug)]
pub enum LoadError {
    /// Errors from `cli.args.parse` (only `OutOfMemory` in practice since
    /// the config tracks diagnostics).
    Args(Error),
    Io(io::Error),
    Xdg(file_load::XdgError),
    Passwd(crate::os::passwd::Error),
    Open(file_load::OpenFileError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Args(e) => write!(f, "{}", e),
            LoadError::Io(e) => write!(f, "{}", e),
            LoadError::Xdg(e) => write!(f, "{}", e),
            LoadError::Passwd(e) => write!(f, "{}", e),
            LoadError::Open(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<Error> for LoadError {
    fn from(e: Error) -> Self {
        LoadError::Args(e)
    }
}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        LoadError::Io(e)
    }
}

impl From<file_load::XdgError> for LoadError {
    fn from(e: file_load::XdgError) -> Self {
        LoadError::Xdg(e)
    }
}

impl From<crate::os::passwd::Error> for LoadError {
    fn from(e: crate::os::passwd::Error) -> Self {
        LoadError::Passwd(e)
    }
}

impl From<file_load::OpenFileError> for LoadError {
    fn from(e: file_load::OpenFileError) -> Self {
        LoadError::Open(e)
    }
}

impl LoadError {
    /// True if this is a "file not found" error (Zig `error.FileNotFound`).
    fn is_file_not_found(&self) -> bool {
        match self {
            LoadError::Open(file_load::OpenFileError::FileNotFound) => true,
            LoadError::Io(e) => e.kind() == io::ErrorKind::NotFound,
            _ => false,
        }
    }
}

impl ParseHooks for Config {
    fn diagnostics_mut(&mut self) -> Option<&mut DiagnosticList> {
        Some(&mut self._diagnostics)
    }

    fn compatibility(key: &str) -> Option<CompatibilityHandler<Self>> {
        compatibility(key)
    }

    /// Callback for src/cli/args.zig to allow us to handle special cases
    /// like `--help` or `-e`. Returns "false" if the CLI parsing should halt.
    fn parse_manually_hook<I: ArgIterator + ?Sized>(
        &mut self,
        arg: &str,
        iter: &mut I,
    ) -> Result<bool, Error> {
        self.parse_manually(arg, &mut HookArgs(iter))
    }
}

/// The iterator interface the `-e` handling needs: the remaining arguments
/// and a location for diagnostics. Implemented by the generic `cli.args`
/// iterator ([`HookArgs`]) and by [`ReplayIterator`].
trait ManualArgs {
    fn next_arg(&mut self, config: &mut Config) -> Option<String>;
    fn location(&self) -> Location;
}

struct HookArgs<'a, I: ArgIterator + ?Sized>(&'a mut I);

impl<I: ArgIterator + ?Sized> ManualArgs for HookArgs<'_, I> {
    fn next_arg(&mut self, _config: &mut Config) -> Option<String> {
        self.0.next()
    }

    fn location(&self) -> Location {
        Location::from_iter(&*self.0)
    }
}

/// An iterator that yields exactly one argument.
struct SingleArg(Option<String>);

impl ArgIterator for SingleArg {
    fn next(&mut self) -> Option<String> {
        self.0.take()
    }
}

impl Config {
    pub fn deinit(&mut self) {
        // Nothing to do: every value owns its memory (Zig frees the arena).
    }

    /// Load the configuration according to the default rules:
    ///
    ///   1. Defaults
    ///   2. XDG config dir
    ///   3. "Application Support" directory (macOS only)
    ///   4. CLI flags
    ///   5. Recursively defined configuration files
    ///
    pub fn load() -> Result<Config, LoadError> {
        let mut result = Config::default();

        // If we have a configuration file in our home directory, parse that first.
        result.load_default_files()?;

        // Parse the config from the CLI args.
        result.load_cli_args()?;

        // Parse the config files that were added from our file and CLI args.
        result.load_recursive_files()?;
        result.finalize()?;

        Ok(result)
    }

    /// The default configuration (Zig `Config.default(alloc)`). This
    /// shadows `<Config as Default>::default`, which only contains the raw
    /// field defaults.
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Config {
        // Build up our basic config
        let mut result: Config = <Config as Default>::default();

        // Add our default keybindings
        result.keybind.init();

        // Add our default command palette entries
        result.command_palette_entry.init();

        // Add our default link for URL detection
        result.link.links.push(crate::input::link::Link {
            regex: Cow::Borrowed(url::REGEX.as_bytes()),
            action: crate::input::link::Action::Open,
            highlight: crate::input::link::Highlight::HoverMods(ctrl_or_super(Mods::EMPTY)),
        });

        result
    }

    /// Load configuration from an iterator that yields values that look like
    /// command-line arguments, i.e. `--key=value`.
    pub fn load_iter<I: ArgIterator + ?Sized>(&mut self, iter: &mut I) -> Result<(), Error> {
        args::parse(self, iter)
    }

    /// Load configuration from the target config file at `path`.
    ///
    /// `path` must be resolved and absolute.
    pub fn load_file(&mut self, path: &str) -> Result<(), LoadError> {
        debug_assert!(std::path::Path::new(path).is_absolute());
        let file = match file_load::open(path) {
            Ok(file) => file,
            Err(file_load::OpenFileError::NotAFile) => {
                log::warn!(
                    target: LOG_TARGET,
                    "config-file {}: not reading because it is not a file",
                    path
                );
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };

        self.load_fs_file(file, path)
    }

    /// Load config from the given File.
    fn load_fs_file(&mut self, file: std::fs::File, path: &str) -> Result<(), LoadError> {
        log::info!("reading configuration file path={}", path);
        let reader = io::BufReader::with_capacity(2048, file);
        self.load_reader(reader, path)
    }

    /// Load config from the given Reader.
    fn load_reader<R: BufRead>(&mut self, mut reader: R, path: &str) -> Result<(), LoadError> {
        'bom: {
            // If the file starts with a UTF-8 byte order mark, skip it.
            // https://en.wikipedia.org/wiki/Byte_order_mark#UTF-8
            const BOM: &[u8] = &[0xef, 0xbb, 0xbf];
            let Ok(buf) = reader.fill_buf() else { break 'bom };
            if buf.len() >= BOM.len() && &buf[..BOM.len()] == BOM {
                log::info!(target: LOG_TARGET, "skipping UTF-8 byte order mark");
                reader.consume(BOM.len());
            }
        }
        let mut iter = args::LineIterator::init(reader);
        iter.filepath = path.to_owned();
        self.load_iter(&mut iter)?;
        let dirname = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .expect("absolute path has a dirname");
        self.expand_paths(&dirname)?;
        Ok(())
    }

    /// Load optional configuration file from `path`. All errors are ignored.
    ///
    /// Returns the action that was taken.
    pub fn load_optional_file(&mut self, path: &str) -> OptionalFileAction {
        match self.load_file(path) {
            Ok(()) => OptionalFileAction::Loaded,
            Err(err) if err.is_file_not_found() => OptionalFileAction::NotFound,
            Err(err) => {
                log::warn!(
                    "error reading optional config file, not loading err={} path={}",
                    err,
                    path
                );

                OptionalFileAction::Error
            }
        }
    }

    /// Load configurations from the default configuration files. The default
    /// configuration file is at `$XDG_CONFIG_HOME/ghostty/config.ghostty`.
    ///
    /// On macOS, `$HOME/Library/Application Support/$CFBundleIdentifier/`
    /// is also loaded.
    ///
    /// The legacy `config` file (without extension) is first loaded,
    /// then `config.ghostty`.
    pub fn load_default_files(&mut self) -> Result<(), LoadError> {
        // Load XDG first
        let legacy_xdg_path = file_load::legacy_default_xdg_path()?;
        let xdg_path = file_load::default_xdg_path()?;
        let xdg_loaded: bool = {
            let legacy_xdg_action = self.load_optional_file(&legacy_xdg_path);
            let xdg_action = self.load_optional_file(&xdg_path);
            if xdg_action != OptionalFileAction::NotFound
                && legacy_xdg_action != OptionalFileAction::NotFound
            {
                log::warn!(
                    target: LOG_TARGET,
                    "both config files `{}` and `{}` exist.",
                    legacy_xdg_path,
                    xdg_path
                );
                log::warn!(target: LOG_TARGET, "loading them both in that order");
                true
            } else {
                xdg_action != OptionalFileAction::NotFound
                    || legacy_xdg_action != OptionalFileAction::NotFound
            }
        };

        // On macOS load the app support directory as well
        #[cfg(target_os = "macos")]
        {
            let legacy_app_support_path = file_load::legacy_default_app_support_path()?;
            let app_support_path = file_load::preferred_app_support_path()?;
            let app_support_loaded: bool = {
                let legacy_app_support_action = self.load_optional_file(&legacy_app_support_path);

                // The app support path and legacy may be the same, since we
                // use the `preferred` call above. If its the same, avoid
                // a double-load.
                let app_support_action = if legacy_app_support_path != app_support_path {
                    self.load_optional_file(&app_support_path)
                } else {
                    OptionalFileAction::NotFound
                };

                if app_support_action != OptionalFileAction::NotFound
                    && legacy_app_support_action != OptionalFileAction::NotFound
                {
                    log::warn!(
                        target: LOG_TARGET,
                        "both config files `{}` and `{}` exist.",
                        legacy_app_support_path,
                        app_support_path
                    );
                    log::warn!(target: LOG_TARGET, "loading them both in that order");
                    true
                } else {
                    app_support_action != OptionalFileAction::NotFound
                        || legacy_app_support_action != OptionalFileAction::NotFound
                }
            };

            // If both files are not found, then we create a template file.
            // For macOS, we only create the template file in the app support
            if !app_support_loaded && !xdg_loaded {
                if let Err(err) = write_config_template(&app_support_path) {
                    log::warn!(target: LOG_TARGET, "error creating template config file err={}", err);
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        if !xdg_loaded {
            if let Err(err) = write_config_template(&xdg_path) {
                log::warn!(target: LOG_TARGET, "error creating template config file err={}", err);
            }
        }

        Ok(())
    }

    /// Load and parse the CLI args.
    pub fn load_cli_args(&mut self) -> Result<(), LoadError> {
        let argv: Vec<std::ffi::CString> = {
            use std::os::unix::ffi::OsStringExt;
            crate::global::args()
                .into_iter()
                .map(|a| std::ffi::CString::new(a.into_vec()).unwrap_or_default())
                .collect()
        };
        self.load_cli_args_from(&argv)
    }

    /// The body of [`Config::load_cli_args`] for the given process
    /// arguments (Zig reads `global.args()`).
    fn load_cli_args_from(&mut self, argv: &[std::ffi::CString]) -> Result<(), LoadError> {
        // Fast-path if we are Linux/BSD and have no args.
        if cfg!(any(target_os = "linux", target_os = "freebsd")) && argv.len() <= 1 {
            return Ok(());
        }

        // On Linux, we have a special case where if the executing
        // program is "xdg-terminal-exec" then we treat all CLI
        // args as if they are a command to execute.
        //
        // In this mode, we also behave slightly differently:
        //
        //   - The initial window title is set to the full command. This
        //     can be used with window managers to modify positioning,
        //     styling, etc. based on the command.
        //
        // See: https://github.com/Vladimir-csp/xdg-terminal-exec
        if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            let vector: Vec<&std::ffi::CStr> = argv.iter().map(|a| a.as_c_str()).collect();
            if let Some(args) = crate::os::xdg::parse_terminal_exec(&vector) {
                // First, we add an artificial "-e" so that if we
                // replay the inputs to rebuild the config (i.e. if
                // a theme is set) then we will get the same behavior.
                self._replay_steps.push(ReplayStep::DashE);

                // Next, take all remaining args and use that to build up
                // a command to execute.
                let mut builder: Vec<StringZ> = Vec::new();
                for arg_raw in args {
                    let copy = StringZ::from(arg_raw.to_string_lossy().into_owned());
                    self._replay_steps.push(ReplayStep::Arg(copy.clone()));
                    builder.push(copy);
                }

                self._xdg_terminal_exec = true;
                self.initial_command = Some(Command::Direct(builder));
                return Ok(());
            }
        }

        // We set config-default-files to true here because this
        // should always be reset so we can detect if it is set
        // in the CLI since it is documented as having no affect
        // from files.
        self.config_default_files = true;

        // Keep track of the replay steps up to this point so we
        // can replay if we are discarding the default files.
        let replay_len_start = self._replay_steps.len();

        // font-family settings set via the CLI overwrite any prior values
        // rather than append. This avoids a UX oddity where you have to
        // specify `font-family=""` to clear the font families.
        self.font_family.overwrite_next = true;
        self.font_family_bold.overwrite_next = true;
        self.font_family_italic.overwrite_next = true;
        self.font_family_bold_italic.overwrite_next = true;

        // Initialize our CLI iterator.
        let process_args = args::ProcessArgs::new(
            argv.iter().map(|a| a.to_string_lossy().into_owned()),
        );
        let mut iter = args::args_iterator(process_args);
        let result = self.load_iter(&mut iter);

        // (Zig `defer`)
        self.font_family.overwrite_next = false;
        self.font_family_bold.overwrite_next = false;
        self.font_family_italic.overwrite_next = false;
        self.font_family_bold_italic.overwrite_next = false;
        result?;

        // If we are not loading the default files, then we need to
        // replay the steps up to this point so that we can rebuild
        // the config without it.
        'reload: {
            if self.config_default_files {
                break 'reload;
            }
            let replay_len_end = self._replay_steps.len();
            if replay_len_end == replay_len_start {
                break 'reload;
            }
            log::info!(
                target: LOG_TARGET,
                "config-default-files unset, discarding configuration from default files"
            );

            let mut new_config = self.clone_empty();
            new_config.load_replay(&self._replay_steps[replay_len_start..replay_len_end])?;
            *self = new_config;
        }

        // Any paths referenced from the CLI are relative to the current working
        // directory.
        let cwd = std::fs::canonicalize(".")?;
        self.expand_paths(&cwd.to_string_lossy())?;
        Ok(())
    }

    /// Load and parse the config files that were added in the "config-file" key.
    pub fn load_recursive_files(&mut self) -> Result<(), LoadError> {
        if self.config_file.value.is_empty() {
            return Ok(());
        }

        // Keeps track of loaded files to prevent cycles.
        let mut loaded: std::collections::HashSet<String> = std::collections::HashSet::new();

        // We need to insert all of our loaded config-file values
        // PRIOR to the "-e" in our replay steps, since everything
        // after "-e" becomes an "initial-command". To do this, we
        // dupe the values if we find it.
        let mut replay_suffix: Vec<ReplayStep> = Vec::new();
        if let Some(i) = self._replay_steps.iter().position(|s| *s == ReplayStep::DashE) {
            replay_suffix.extend_from_slice(&self._replay_steps[i..]);

            // Remove our old values.
            self._replay_steps.truncate(i);
        }

        // We must use a while below and not a for(items) because we
        // may add items to the list while iterating for recursive
        // config-file entries.
        let mut i: usize = 0;
        while i < self.config_file.value.len() {
            let (path, optional) = match &self.config_file.value[i] {
                Path::Optional(path) => (path.as_str().to_owned(), true),
                Path::Required(path) => (path.as_str().to_owned(), false),
            };
            i += 1;

            // Error paths
            if path.is_empty() {
                continue;
            }

            // All paths should already be absolute at this point because
            // they're fixed up after each load.
            debug_assert!(std::path::Path::new(&path).is_absolute());

            // We must only load a unique file once
            if !loaded.insert(path.clone()) {
                let diag = Diagnostic {
                    message: format!("config-file {}: cycle detected", path).into(),
                    ..Default::default()
                };

                self._diagnostics.append(diag.clone());
                self._replay_steps.push(ReplayStep::Diagnostic(diag));
                continue;
            }

            let file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(err) => {
                    if err.kind() != io::ErrorKind::NotFound || !optional {
                        let diag = Diagnostic {
                            message: format!(
                                "error opening config-file {}: error.{}",
                                path,
                                super::path::io_error_name(&err)
                            )
                            .into(),
                            ..Default::default()
                        };

                        self._diagnostics.append(diag.clone());
                        self._replay_steps.push(ReplayStep::Diagnostic(diag));
                    }
                    continue;
                }
            };

            let stat = file.metadata()?;
            if !stat.file_type().is_file() {
                let diag = Diagnostic {
                    message: format!(
                        "config-file {}: not reading because file type is {}",
                        path,
                        file_kind_name(&stat.file_type())
                    )
                    .into(),
                    ..Default::default()
                };

                self._diagnostics.append(diag.clone());
                self._replay_steps.push(ReplayStep::Diagnostic(diag));
                continue;
            }

            self.load_fs_file(file, &path)?;
        }

        // If we have a suffix, add that back.
        if !replay_suffix.is_empty() {
            self._replay_steps.extend(replay_suffix);
        }

        Ok(())
    }

    /// Change the state of conditionals and reload the configuration
    /// based on the new state. This returns a new configuration based
    /// on the new state. The caller must free the old configuration if they
    /// wish.
    ///
    /// This returns null if the conditional state would result in no changes
    /// to the configuration. In this case, the caller can continue to use
    /// the existing configuration or clone if they want a copy.
    ///
    /// This doesn't re-read any files, it just re-applies the same
    /// configuration with the new conditional state. Importantly, this means
    /// that if you change the conditional state and the user in the interim
    /// deleted a file that was referenced in the configuration, then the
    /// configuration can still be reloaded.
    pub fn change_conditional_state(
        &self,
        new: conditional::State,
    ) -> Result<Option<Config>, LoadError> {
        // If the conditional state between the old and new is the same,
        // then we don't need to do anything.
        'relevant: {
            for &field in <conditional::Key as CliEnum>::FIELD_NAMES {
                let key = conditional::Key::from_tag(field).expect("conditional key");

                // Conditional set contains the keys that this config uses. So we
                // only continue if we use this key.
                let differs = match key {
                    conditional::Key::Theme => self._conditional_state.theme != new.theme,
                    conditional::Key::Os => self._conditional_state.os != new.os,
                };
                if self._conditional_set.contains(key) && differs {
                    break 'relevant;
                }
            }

            // If we got here, then we didn't find any differences between
            // the old and new conditional state that would affect the
            // configuration.
            return Ok(None);
        }

        // Create our new configuration
        let mut new_config = self.clone_empty();

        // Set our conditional state so the replay below can use it
        new_config._conditional_state = new;

        // Replay all of our steps to rebuild the configuration
        new_config.load_replay(&self._replay_steps)?;
        new_config.finalize()?;

        Ok(Some(new_config))
    }

    /// Expand the relative paths in config-files to be absolute paths
    /// relative to the base directory.
    fn expand_paths(&mut self, base: &str) -> io::Result<()> {
        // Keep track of this step for replays
        self._replay_steps.push(ReplayStep::Expand(base.to_owned()));

        // Expand all of our paths (Zig iterates every field whose type is
        // Path, ?Path, RepeatablePath or ?RepeatablePath, in field order).
        if let Some(path) = &mut self.background_image {
            path.expand(base, &mut self._diagnostics)?;
        }
        self.config_file.expand(base, &mut self._diagnostics)?;
        self.custom_shader.expand(base, &mut self._diagnostics)?;
        if let Some(path) = &mut self.bell_audio_path {
            path.expand(base, &mut self._diagnostics)?;
        }
        self.gtk_custom_css.expand(base, &mut self._diagnostics)?;
        Ok(())
    }

    fn load_theme(&mut self, theme: &Theme) -> Result<(), LoadError> {
        // Load the correct theme depending on the conditional state.
        // Dark/light themes were programmed prior to conditional configuration
        // so when we introduce that we probably want to replace this.
        let name: &str = match self._conditional_state.theme {
            conditional::Theme::Light => &theme.light,
            conditional::Theme::Dark => &theme.dark,
        };

        // Find our theme file and open it. See the open function for details.
        let Some(themefile) = themepkg::open(name, &mut self._diagnostics) else {
            return Ok(());
        };
        let path = themefile.path;
        let file = themefile.file;

        // From this point onwards, we load the theme and do a bit of a dance
        // to achieve two separate goals:
        //
        //   (1) We want the theme to be loaded and our existing config to
        //       override the theme. So we need to load the theme and apply
        //       our config on top of it.
        //
        //   (2) We want to free existing memory that we aren't using anymore
        //       as a result of reloading the configuration.
        //
        // Point 2 is strictly a result of aur approach to point 1, but it is
        // a nice property to have to limit memory bloat as much as possible.

        // Load into a new configuration so that we can free the existing memory.
        let mut new_config = self.clone_empty();

        // Load our theme
        let reader = io::BufReader::with_capacity(2048, file);
        let mut iter = args::LineIterator::init(reader);
        iter.filepath = path;
        new_config.load_iter(&mut iter)?;

        // Setup our replay to be conditional.
        let theme_value = self._conditional_state.theme.tag_name();
        for item in new_config._replay_steps.iter_mut() {
            match item {
                ReplayStep::Expand(_) | ReplayStep::Diagnostic(_) => {}

                // If we see "-e" then we do NOT make the following arguments
                // conditional since they are supposed to be part of the
                // initial command.
                ReplayStep::DashE => break,

                // Change our arg to be conditional on our theme.
                ReplayStep::Arg(v) => {
                    let conds = vec![Conditional {
                        key: conditional::Key::Theme,
                        op: conditional::Op::Eq,
                        value: theme_value.to_owned(),
                    }];
                    *item = ReplayStep::ConditionalArg {
                        conditions: conds,
                        arg: v.as_str().to_owned(),
                    };
                }

                ReplayStep::ConditionalArg { conditions, arg } => {
                    let mut conds = Vec::with_capacity(conditions.len() + 1);
                    conds.push(Conditional {
                        key: conditional::Key::Theme,
                        op: conditional::Op::Eq,
                        value: theme_value.to_owned(),
                    });
                    conds.extend_from_slice(conditions);
                    *item = ReplayStep::ConditionalArg {
                        conditions: conds,
                        arg: std::mem::take(arg),
                    };
                }
            }
        }

        // Replay our previous inputs so that we can override values
        // from the theme.
        new_config.load_replay(&self._replay_steps)?;

        // Success, swap our new config in and free the old.
        *self = new_config;
        Ok(())
    }

    /// Call this once after you are done setting configuration. This
    /// is idempotent but will waste memory if called multiple times.
    pub fn finalize(&mut self) -> Result<(), LoadError> {
        // We always load the theme first because it may set other fields
        // in our config.
        if let Some(theme) = &mut self.theme {
            theme.finalize();
            let theme = theme.clone();
            let different = theme.light != theme.dark;

            // Warning: loadTheme will deinit our existing config and replace
            // it so all memory from self prior to this point will be freed.
            self.load_theme(&theme)?;

            // If we have different light vs dark mode themes, disable
            // window-theme = auto since that breaks it.
            if different {
                // This setting doesn't make sense with different light/dark themes
                // because it'll force the theme based on the Ghostty theme.
                if self.window_theme == WindowTheme::Auto {
                    self.window_theme = WindowTheme::System;
                }

                // Mark that we use a conditional theme
                self._conditional_set.insert(conditional::Key::Theme);
            }
        }

        // Used for a variety of defaults. See the function docs as well the
        // specific variable use sites for more details.
        let probable_cli = probable_cli_environment();

        // If we have a font-family set and don't set the others, default
        // the others to the font family. This way, if someone does
        // --font-family=foo, then we try to get the stylized versions of
        // "foo" as well.
        if self.font_family.count() > 0 {
            if self.font_family_bold.count() == 0 {
                self.font_family_bold = self.font_family.clone();
            }
            if self.font_family_italic.count() == 0 {
                self.font_family_italic = self.font_family.clone();
            }
            if self.font_family_bold_italic.count() == 0 {
                self.font_family_bold_italic = self.font_family.clone();
            }
        }

        // Prevent setting TERM to an empty string
        if self.term.is_empty() {
            // HACK: See comment above at definition
            self.term = String::from("xterm-ghostty");
        }

        // The default for the working directory depends on the system.
        let mut wd: WorkingDirectory = match &self.working_directory {
            Some(wd) => wd.clone(),
            None if probable_cli => WorkingDirectory::Inherit,
            None => WorkingDirectory::Home,
        };

        // If we are missing either a command or home directory, we need
        // to look up defaults which is kind of expensive. We only do this
        // on desktop.
        if !cfg!(target_arch = "wasm32") && !cfg!(test) {
            'command: {
                if !(self.command.is_none() || wd == WorkingDirectory::Home) {
                    break 'command;
                }

                // First look up the command using the SHELL env var if needed.
                // We don't do this in flatpak because SHELL in Flatpak is always
                // set to /bin/sh.
                if let Some(cmd) = &self.command {
                    log::info!(target: LOG_TARGET, "shell src=config value={}", cmd.string());
                } else {
                    'shell_env: {
                        // Flatpak always gets its shell from outside the sandbox
                        if crate::os::is_flatpak() {
                            break 'shell_env;
                        }

                        // If we were launched from the desktop, our SHELL env var
                        // will represent our SHELL at login time. We only want to
                        // read from SHELL if we're in a probable CLI environment.
                        if !probable_cli {
                            break 'shell_env;
                        }

                        let Ok(value) = std::env::var("SHELL") else {
                            break 'shell_env;
                        };

                        log::info!(target: LOG_TARGET, "default shell source=env value={}", value);

                        self.command = Some(Command::Shell(StringZ::from(value)));

                        // If we don't need the working directory, then we can exit now.
                        if wd != WorkingDirectory::Home {
                            break 'command;
                        }
                    }
                }

                if cfg!(windows) {
                    if self.command.is_none() {
                        log::warn!(
                            target: LOG_TARGET,
                            "no default shell found, will default to using cmd"
                        );
                        self.command = Some(Command::Shell(StringZ::from("cmd.exe")));
                    }

                    if wd == WorkingDirectory::Home {
                        let environ_map = crate::os::environ_map();
                        let mut buf = [0u8; MAX_PATH_BYTES];
                        match crate::os::home(&environ_map, &mut buf) {
                            Ok(Some(home)) => wd = WorkingDirectory::Path(home.to_owned()),
                            _ => wd = WorkingDirectory::Inherit,
                        }
                    }
                } else {
                    // We need the passwd entry for the remainder
                    let pw = crate::os::passwd::get()?;
                    if self.command.is_none() {
                        if let Some(sh) = &pw.shell {
                            log::info!(target: LOG_TARGET, "default shell src=passwd value={}", sh);
                            self.command = Some(Command::Shell(StringZ::from(sh.as_str())));
                        }
                    }

                    if wd == WorkingDirectory::Home {
                        if let Some(home) = &pw.home {
                            log::info!(
                                target: LOG_TARGET,
                                "default working directory src=passwd value={}",
                                home
                            );
                            wd = WorkingDirectory::Path(home.clone());
                        } else {
                            wd = WorkingDirectory::Inherit;
                        }
                    }

                    if self.command.is_none() {
                        log::warn!(
                            target: LOG_TARGET,
                            "no default shell found, will default to using sh"
                        );
                    }
                }
            }
        }
        wd.finalize();
        self.working_directory = Some(wd);

        // Apprt-specific defaults
        match crate::build_config::APP_RUNTIME {
            crate::apprt::runtime::Runtime::None => {}
            crate::apprt::runtime::Runtime::Gtk => {
                match self.gtk_single_instance {
                    GtkSingleInstance::True | GtkSingleInstance::False => {}

                    // For detection, we assume single instance unless we're
                    // in a CLI environment, then we disable single instance.
                    GtkSingleInstance::Detect => {
                        self.gtk_single_instance = if probable_cli {
                            GtkSingleInstance::False
                        } else {
                            GtkSingleInstance::True
                        }
                    }
                }
            }
        }

        // Default our click interval
        if self.click_repeat_interval == 0 && !cfg!(test) {
            self.click_repeat_interval = crate::os::click_interval().unwrap_or(500);
        }

        // Clamp our mouse scroll multiplier
        self.mouse_scroll_multiplier.precision =
            f64::min(10_000.0, f64::max(0.01, self.mouse_scroll_multiplier.precision));
        self.mouse_scroll_multiplier.discrete =
            f64::min(10_000.0, f64::max(0.01, self.mouse_scroll_multiplier.discrete));

        // Clamp our split opacity
        self.unfocused_split_opacity = f64::min(1.0, f64::max(0.15, self.unfocused_split_opacity));

        // Clamp our contrast
        self.minimum_contrast = f64::min(21.0, f64::max(1.0, self.minimum_contrast));

        // Minimum window size
        if self.window_width > 0 {
            self.window_width = u32::max(10, self.window_width);
        }
        if self.window_height > 0 {
            self.window_height = u32::max(4, self.window_height);
        }

        // If URLs are disabled, cut off the first link. The first link is
        // always the URL matcher.
        if !self.link_url {
            self.link.links.remove(0);
        }

        // We warn when the quit-after-last-window-closed-delay is set to a very
        // short value because it can cause Ghostty to quit before the first
        // window is even shown.
        if let Some(duration) = self.quit_after_last_window_closed_delay {
            if duration.duration < 5 * NS_PER_S {
                log::warn!(
                    target: LOG_TARGET,
                    "quit-after-last-window-closed-delay is set to a very short value ({}), which might cause problems",
                    duration
                );
            }
        }

        // We can't set this as a struct default because our config is
        // loaded in environments where a build config isn't available.
        if self.auto_update_channel.is_none() {
            self.auto_update_channel = Some(crate::build_config::RELEASE_CHANNEL);
        }

        self.faint_opacity = self.faint_opacity.clamp(0.0, 1.0);

        // Finalize key remapping set for efficient lookups
        self.key_remap.finalize();

        Ok(())
    }

    /// Zig `parseManuallyHook`, shared by the generic `cli.args` hook and
    /// the replay loop. Returns "false" if the CLI parsing should halt.
    fn parse_manually(&mut self, arg: &str, iter: &mut dyn ManualArgs) -> Result<bool, Error> {
        if arg == "-e" {
            // Add the special -e marker. This prevents:
            // (1) config-file from adding args to the end (see #2908)
            // (2) dark/light theme from making this conditional
            self._replay_steps.push(ReplayStep::DashE);

            // Build up the command. We don't clean this up because we take
            // ownership in our allocator.
            let mut command: Vec<StringZ> = Vec::new();

            while let Some(param) = iter.next_arg(self) {
                let copy = StringZ::from(param);
                self._replay_steps.push(ReplayStep::Arg(copy.clone()));
                command.push(copy);
            }

            if command.is_empty() {
                let location = iter.location();
                self._diagnostics.append(Diagnostic {
                    location,
                    message: format!("missing command after {}", arg).into(),
                    ..Default::default()
                });

                return Ok(false);
            }

            // See "command" docs for the implied configurations and why.
            self.initial_command = Some(Command::Direct(command));
            self.gtk_single_instance = GtkSingleInstance::False;
            self.quit_after_last_window_closed = true;
            self.quit_after_last_window_closed_delay = None;
            if self.shell_integration != ShellIntegration::None {
                self.shell_integration = ShellIntegration::Detect;
            }

            // Do not continue, we consumed everything.
            return Ok(false);
        }

        // Keep track of our input args for replay
        self._replay_steps.push(ReplayStep::Arg(StringZ::from(arg)));

        // If we didn't find a special case, continue parsing normally
        Ok(true)
    }

    /// Replay the given steps into this configuration (Zig:
    /// `loadIter(alloc, &Replay.iterator(steps, self))`).
    fn load_replay(&mut self, steps: &[ReplayStep]) -> Result<(), Error> {
        let mut it = Replay::iterator(steps);
        while let Some(arg) = it.next(self) {
            // The manual hook: "-e" consumes the rest of the iterator.
            if arg == "-e" {
                self.parse_manually(&arg, &mut it)?;
                return Ok(());
            }

            // Everything else goes through the normal argument parser
            // (which calls the hook again to record the replay step).
            args::parse(self, &mut SingleArg(Some(arg)))?;
        }

        Ok(())
    }

    /// Add a diagnostic message to the config with the given string.
    /// This is always added with a location of "none".
    pub fn add_diagnostic_fmt(&mut self, args: fmt::Arguments<'_>) {
        self._diagnostics.append(Diagnostic {
            message: fmt::format(args).into(),
            ..Default::default()
        });
    }

    /// Create a shallow copy of this config. This will share all the memory
    /// allocated with the previous config but will have a new arena for
    /// any changes or new allocations. The config should have `deinit`
    /// called when it is complete.
    ///
    /// Beware: these shallow clones are not meant for a long lifetime,
    /// they are just meant to exist temporarily for the duration of some
    /// modifications. It is very important that the original config not
    /// be deallocated while shallow clones exist.
    ///
    /// (Rust: values own their memory so this is a full copy.)
    pub fn shallow_clone(&self) -> Config {
        <Config as Clone>::clone(self)
    }

    /// Create a copy of the metadata of this configuration but without
    /// the actual values. Metadata includes conditional state.
    pub fn clone_empty(&self) -> Config {
        let mut result = Config::default();
        result._conditional_state = self._conditional_state;
        result
    }

    /// Create a copy of this configuration.
    ///
    /// This will not re-read referenced configuration files and operates
    /// purely in-memory.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Config {
        // Start with an empty config, then copy our values (all `Key`
        // fields), diagnostics, replay steps (exactly, to also preserve
        // the exact conditionals required for some steps) and the
        // conditional set. Zig's field-by-field copy is the derived clone;
        // only `_xdg-terminal-exec` is not copied.
        let mut result = <Config as Clone>::clone(self);
        result._xdg_terminal_exec = false;
        debug_assert_eq!(result._replay_steps.len(), self._replay_steps.len());
        result
    }

    /// Returns an iterator that goes through each changed field from
    /// old to new. The order of old or new do not matter.
    pub fn change_iterator<'a>(&'a self, new: &'a Config) -> ChangeIterator<'a> {
        ChangeIterator {
            old: self,
            new,
            i: 0,
        }
    }

    /// Returns true if the given key has changed from old to new.
    pub fn changed(&self, new: &Config, key: Key) -> bool {
        self.field_changed(new, key.index())
    }

    /// Returns true if the given key event would trigger a keybinding
    /// if it were to be processed. This is useful for determining if
    /// a key event should be sent to the terminal or not.
    pub fn key_event_is_binding(&self, event: &crate::input::key::KeyEvent<'_>) -> bool {
        match event.action {
            crate::input::key::Action::Release => return false,
            crate::input::key::Action::Press | crate::input::key::Action::Repeat => {}
        }

        // If we have a keybinding for this event then we return true.
        self.keybind.set.get_event(event).is_some()
    }
}

/// This yields a key for every changed field between old and new.
pub struct ChangeIterator<'a> {
    pub old: &'a Config,
    pub new: &'a Config,
    pub i: usize,
}

impl Iterator for ChangeIterator<'_> {
    type Item = Key;

    fn next(&mut self) -> Option<Key> {
        let fields = <Key as CliEnum>::FIELD_NAMES;
        while self.i < fields.len() {
            let key = Key::from_index(self.i);
            self.i += 1;
            if self.old.changed(self.new, key) {
                return Some(key);
            }
        }

        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionalFileAction {
    Loaded,
    NotFound,
    Error,
}

/// Zig `@tagName(stat.kind)`.
fn file_kind_name(ft: &std::fs::FileType) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if ft.is_block_device() {
            return "block_device";
        }
        if ft.is_char_device() {
            return "character_device";
        }
        if ft.is_fifo() {
            return "named_pipe";
        }
        if ft.is_socket() {
            return "unix_domain_socket";
        }
    }
    if ft.is_dir() {
        "directory"
    } else if ft.is_symlink() {
        "sym_link"
    } else if ft.is_file() {
        "file"
    } else {
        "unknown"
    }
}

fn write_config_template(path: &str) -> io::Result<()> {
    log::info!(target: LOG_TARGET, "creating template config file: path={}", path);
    if let Some(dir_path) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir_path)?;
    }
    let template = include_str!("config-template").replace("{[path]s}", path);
    std::fs::write(path, template)
}

/// Expand tilde paths to absolute paths to the user's home directory.
/// If expansion fails, an error is logged and the original path is returned.
fn expand_home(path: &str) -> Cow<'_, str> {
    if !path.starts_with("~/") {
        return Cow::Borrowed(path);
    }

    let environ_map = crate::os::environ_map();
    let mut buf = [0u8; MAX_PATH_BYTES];
    match crate::os::expand_home(&environ_map, path, &mut buf) {
        Ok(expanded) => Cow::Owned(expanded.to_owned()),
        Err(err) => {
            log::warn!(
                target: LOG_TARGET,
                "failed to expand home directory in path \"{}\": {}",
                path,
                err
            );
            Cow::Borrowed(path)
        }
    }
}

fn compat_gtk_tabs_location(cfg: &mut Config, key: &str, value: Option<&str>) -> bool {
    debug_assert_eq!(key, "gtk-tabs-location");

    if value.unwrap_or("") == "hidden" {
        cfg.window_show_tab_bar = WindowShowTabBar::Never;
        return true;
    }

    false
}

fn compat_gtk_single_instance(cfg: &mut Config, key: &str, value: Option<&str>) -> bool {
    debug_assert_eq!(key, "gtk-single-instance");

    if value.unwrap_or("") == "desktop" {
        cfg.gtk_single_instance = GtkSingleInstance::Detect;
        return true;
    }

    false
}

fn compat_cursor_invert_fg_bg(cfg: &mut Config, key: &str, value_: Option<&str>) -> bool {
    debug_assert_eq!(key, "cursor-invert-fg-bg");

    // We don't do anything if the value is unset, which is technically
    // not EXACTLY the same as prior behavior since it would fallback
    // to doing whatever cursor-color/cursor-text were set to, but
    // I don't want to store what that is separately so this is close
    // enough.
    //
    // Realistically, these fields were mutually exclusive so anyone
    // relying on that behavior should just upgrade to the new
    // cursor-color/cursor-text fields.
    let Ok(set) = args::parse_bool(value_.unwrap_or("t")) else {
        return false;
    };
    if set {
        cfg.cursor_color = Some(TerminalColor::CellForeground);
        cfg.cursor_text = Some(TerminalColor::CellBackground);
    }

    true
}

fn compat_selection_invert_fg_bg(cfg: &mut Config, key: &str, value_: Option<&str>) -> bool {
    debug_assert_eq!(key, "selection-invert-fg-bg");

    let Ok(set) = args::parse_bool(value_.unwrap_or("t")) else {
        return false;
    };
    if set {
        cfg.selection_foreground = Some(TerminalColor::CellBackground);
        cfg.selection_background = Some(TerminalColor::CellForeground);
    }

    true
}

fn compat_bold_is_bright(cfg: &mut Config, key: &str, value_: Option<&str>) -> bool {
    debug_assert_eq!(key, "bold-is-bright");

    let Ok(isset) = args::parse_bool(value_.unwrap_or("t")) else {
        return false;
    };
    if isset {
        cfg.bold_color = Some(BoldColor::Bright);
    }

    true
}

fn compat_macos_dock_drop_behavior(cfg: &mut Config, key: &str, value: Option<&str>) -> bool {
    debug_assert_eq!(key, "macos-dock-drop-behavior");

    if value.unwrap_or("") == "window" {
        cfg.macos_dock_drop_behavior = MacOSDockDropBehavior::NewWindow;
        return true;
    }

    false
}

fn compat_copy_on_select(cfg: &mut Config, key: &str, value: Option<&str>) -> bool {
    debug_assert_eq!(key, "copy-on-select");

    if value.unwrap_or("") == "true" {
        cfg.copy_on_select = if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            CopyOnSelect::Primary
        } else {
            CopyOnSelect::Clipboard
        };
        return true;
    }

    if value.unwrap_or("") == "false" {
        cfg.copy_on_select = CopyOnSelect::None;
        return true;
    }

    false
}

/// This runs a heuristic to determine if we are likely running
/// Ghostty in a CLI environment. We need this to change some behaviors.
/// We should keep the set of behaviors that depend on this as small
/// as possible because magic sucks, but each place is well documented.
fn probable_cli_environment() -> bool {
    // Windows has its own problems, just ignore it for now since
    // its not a real supported target and GTK via WSL2 assuming
    // single instance is probably fine.
    if cfg!(windows) {
        return false;
    }

    // On macOS, we don't want to detect `open` calls as CLI envs.
    // Our desktop detection on macOS is very accurate due to how
    // processes are launched on macOS, so if we detect we're launched
    // from the app bundle then we're not in a CLI environment.
    if cfg!(target_os = "macos") && crate::os::launched_from_desktop() {
        return false;
    }

    // If we have TERM_PROGRAM set to a non-empty value, we assume a graphical
    // terminal environment.
    if crate::global::environ().get("TERM_PROGRAM").is_some_and(|v| !v.is_empty()) {
        return true;
    }

    // CLI arguments makes things probable
    if crate::global::args().len() > 1 {
        return true;
    }

    // Unlikely CLI environment
    false
}

/// This is used to "replay" the configuration. See loadTheme for details.
pub struct Replay;

/// A replay step (Zig `Replay.Step`).
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayStep {
    /// An argument to parse as if it came from the CLI or file.
    Arg(StringZ),

    /// A base path to expand relative paths against.
    Expand(String),

    /// A conditional argument. This arg is parsed only if all
    /// conditions match (an "AND"). An "OR" can be achieved by
    /// having multiple conditional arg entries.
    ConditionalArg {
        conditions: Vec<Conditional>,
        arg: String,
    },

    /// A diagnostic to be added to the new configuration when
    /// replayed. This should only be used for diagnostics that won't
    /// be reproduced during playback. For example, `config-file`
    /// errors are not reloaded so they should be added here.
    ///
    /// Diagnostics cannot be conditional. They are always present
    /// even if the conditionals don't match. This helps users find
    /// errors in their configuration.
    Diagnostic(Diagnostic),

    /// The start of a "-e" argument. This marks the end of
    /// traditional configuration and the beginning of the
    /// "-e" initial command magic. This is separate from "arg"
    /// because there are some behaviors unique to this (i.e.
    /// we want to keep this at the end for config-file).
    ///
    /// Note: when "-e" is used, ONLY this is present and
    /// not an additional "arg" with "-e" value.
    DashE,
}

/// Zig `Replay.Iterator`. The config being replayed into is passed to
/// [`ReplayIterator::next`] instead of being stored.
pub struct ReplayIterator<'a> {
    pub slice: &'a [ReplayStep],
    pub idx: usize,
}

impl Replay {
    /// Construct a Replay iterator from a slice of replay elements.
    /// This can be used with args.parse and handles intermediate
    /// steps such as expanding relative paths.
    pub fn iterator(slice: &[ReplayStep]) -> ReplayIterator<'_> {
        ReplayIterator { slice, idx: 0 }
    }
}

impl ReplayIterator<'_> {
    pub fn next(&mut self, config: &mut Config) -> Option<String> {
        loop {
            if self.idx >= self.slice.len() {
                return None;
            }
            let step = &self.slice[self.idx];
            self.idx += 1;
            match step {
                ReplayStep::Expand(base) => {
                    if let Err(err) = config.expand_paths(base) {
                        // This shouldn't happen because to reach this step
                        // means that it succeeded before. Its possible since
                        // expanding paths is a side effect process that the
                        // world state changed and we can't expand anymore.
                        // In that really unfortunate case, we log a warning.
                        log::warn!(target: LOG_TARGET, "error expanding paths err={}", err);
                    }
                }

                ReplayStep::Diagnostic(diag) => {
                    // Best effort to clone and append the diagnostic.
                    config._diagnostics.append(diag.clone());
                }

                ReplayStep::ConditionalArg { conditions, arg } => {
                    // All conditions must match.
                    if conditions
                        .iter()
                        .all(|cond| config._conditional_state.r#match(cond))
                    {
                        return Some(arg.clone());
                    }
                }

                ReplayStep::Arg(arg) => return Some(arg.as_str().to_owned()),
                ReplayStep::DashE => return Some("-e".to_owned()),
            }
        }
    }
}

impl ManualArgs for ReplayIterator<'_> {
    fn next_arg(&mut self, config: &mut Config) -> Option<String> {
        self.next(config)
    }

    fn location(&self) -> Location {
        // The replay iterator has no location.
        Location::None
    }
}

// ---------------------------------------------------------------------------
// Config/formatter glue for field types defined in other modules.
// ---------------------------------------------------------------------------

impl_cli_enum!(CursorStyle {
    Bar => "bar",
    Block => "block",
    Underline => "underline",
    BlockHollow => "block_hollow",
});

impl_cli_enum!(OptionAsAlt {
    False => "false",
    True => "true",
    Left => "left",
    Right => "right",
});

impl_cli_enum!(WindowPaddingBalance {
    False => "false",
    True => "true",
    Equal => "equal",
});

impl ParseCli for MetricModifier {
    /// So it works with the config framework.
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        MetricModifier::parse_cli(input).map_err(|err| match err {
            crate::font::metrics::ModifierParseError::ValueRequired => Error::ValueRequired,
            crate::font::metrics::ModifierParseError::InvalidFormat => Error::Other("InvalidFormat"),
        })
    }
}

impl FormatEntry for MetricModifier {
    /// Used by config formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        MetricModifier::format_entry(self, f)
    }
}

impl ParseCli for KeyRemapSet {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        KeyRemapSet::parse_cli(self, input.map(str::as_bytes)).map_err(|_| Error::InvalidValue)
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        let mut v = KeyRemapSet::empty();
        <KeyRemapSet as ParseCli>::parse_cli(&mut v, input)?;
        Ok(v)
    }
}

impl FormatEntry for KeyRemapSet {
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        KeyRemapSet::format_entry(self, f)
    }
}

// ---------------------------------------------------------------------------
// Field types
// ---------------------------------------------------------------------------

cli_enum! {
    /// Valid values for confirm-close-surface
    /// c_int because it needs to be extern compatible
    /// If this is changed, you must also update ghostty.h
    #[repr(C)]
    pub enum ConfirmCloseSurface {
        False => "false",
        True => "true",
        Always => "always",
    }
}

cli_enum! {
    /// Valid values for custom-shader-animation
    /// c_int because it needs to be extern compatible
    /// If this is changed, you must also update ghostty.h
    #[repr(C)]
    pub enum CustomShaderAnimation {
        False => "false",
        True => "true",
        Always => "always",
    }
}

cli_enum! {
    /// Valid values for macos-non-native-fullscreen
    /// c_int because it needs to be extern compatible
    /// If this is changed, you must also update ghostty.h
    #[repr(C)]
    pub enum NonNativeFullscreen {
        False => "false",
        True => "true",
        VisibleMenu => "visible-menu",
        PaddedNotch => "padded-notch",
    }
}

cli_enum! {
    /// Valid values for fullscreen config option
    /// c_int because it needs to be extern compatible
    /// If this is changed, you must also update ghostty.h
    #[repr(C)]
    pub enum Fullscreen {
        False => "false",
        True => "true",
        NonNative => "non-native",
        NonNativeVisibleMenu => "non-native-visible-menu",
        NonNativePaddedNotch => "non-native-padded-notch",
    }
}

cli_enum! {
    pub enum WindowPaddingColor {
        Background => "background",
        Extend => "extend",
        ExtendAlways => "extend-always",
    }
}

cli_enum! {
    pub enum WindowSubtitle {
        False => "false",
        WorkingDirectory => "working-directory",
    }
}

cli_enum! {
    pub enum LinkPreviews {
        False => "false",
        True => "true",
        Osc8 => "osc8",
    }
}

/// See working-directory
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkingDirectory {
    /// Resolve to the current user's home directory during config finalize.
    Home,

    /// Inherit the working directory from the launching process.
    Inherit,

    /// Use an explicit working directory path. This may be not be
    /// expanded until finalize is called.
    Path(String),
}

/// Zig `std.ascii.whitespace`.
const ASCII_WHITESPACE: &str = " \t\n\r\x0b\x0c";

impl WorkingDirectory {
    fn parse(input_: Option<&str>) -> Result<WorkingDirectory, Error> {
        let mut input = input_.ok_or(Error::ValueRequired)?;
        input = args::trim(input, ASCII_WHITESPACE);
        if input.is_empty() {
            return Err(Error::ValueRequired);
        }

        // Match path.zig behavior for quoted values.
        if input.len() >= 2 && input.starts_with('"') && input.ends_with('"') {
            input = &input[1..input.len() - 1];
        }

        if input == "home" {
            return Ok(WorkingDirectory::Home);
        }

        if input == "inherit" {
            return Ok(WorkingDirectory::Inherit);
        }

        Ok(WorkingDirectory::Path(input.to_owned()))
    }

    /// Expand tilde paths in .path values.
    pub fn finalize(&mut self) {
        let path = match self {
            WorkingDirectory::Path(path) => path.clone(),
            _ => return,
        };

        let expanded = expand_home(&path);

        if expanded == path {
            return;
        }
        *self = WorkingDirectory::Path(expanded.into_owned());
    }

    pub fn value(&self) -> Option<&str> {
        match self {
            WorkingDirectory::Path(path) => Some(path),
            WorkingDirectory::Home | WorkingDirectory::Inherit => None,
        }
    }

    /// The Zig tag name.
    pub fn tag_name(&self) -> &'static str {
        match self {
            WorkingDirectory::Home => "home",
            WorkingDirectory::Inherit => "inherit",
            WorkingDirectory::Path(_) => "path",
        }
    }
}

impl ParseCli for WorkingDirectory {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        *self = WorkingDirectory::parse(input)?;
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        WorkingDirectory::parse(input)
    }
}

impl FormatEntry for WorkingDirectory {
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        match self {
            WorkingDirectory::Home | WorkingDirectory::Inherit => f.format_entry(self.tag_name()),
            WorkingDirectory::Path(path) => f.format_entry(path.as_str()),
        }
    }
}

/// Color represents a color using RGB.
///
/// This is a packed struct so that the C API to read color values just
/// works by setting it to a C integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// ghostty_config_color_s (Zig `Color.C`)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorC {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub fn cval(self) -> ColorC {
        ColorC {
            r: self.r,
            g: self.g,
            b: self.b,
        }
    }

    /// Convert this to the terminal RGB struct
    pub fn to_terminal_rgb(self) -> terminal_color::Rgb {
        terminal_color::Rgb {
            r: self.r,
            g: self.g,
            b: self.b,
        }
    }

    pub fn parse_cli(input_: Option<&str>) -> Result<Color, Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        let rgb = terminal_color::Rgb::parse(input.as_bytes()).map_err(|_| Error::InvalidValue)?;
        Ok(Color {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        })
    }

    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(self, other: Color) -> bool {
        self == other
    }

    /// Format the color as a string.
    pub fn format_buf(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl ParseCli for Color {
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        Color::parse_cli(input)
    }
}

impl FormatEntry for Color {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        f.format_entry(self.format_buf().as_str())
    }
}

/// Represents color values that can also reference special color
/// values such as "cell-foreground" or "cell-background".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColor {
    Color(Color),
    CellForeground,
    CellBackground,
}

impl TerminalColor {
    pub fn parse_cli(input_: Option<&str>) -> Result<TerminalColor, Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        if input == "cell-foreground" {
            return Ok(TerminalColor::CellForeground);
        }
        if input == "cell-background" {
            return Ok(TerminalColor::CellBackground);
        }
        Ok(TerminalColor::Color(Color::parse_cli(Some(input))?))
    }

    pub fn to_terminal_rgb(self) -> Option<terminal_color::Rgb> {
        match self {
            TerminalColor::Color(v) => Some(v.to_terminal_rgb()),
            TerminalColor::CellForeground | TerminalColor::CellBackground => None,
        }
    }
}

impl ParseCli for TerminalColor {
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        TerminalColor::parse_cli(input)
    }
}

impl FormatEntry for TerminalColor {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        match self {
            TerminalColor::Color(c) => c.format_entry(f),
            TerminalColor::CellForeground => f.format_entry("cell-foreground"),
            TerminalColor::CellBackground => f.format_entry("cell-background"),
        }
    }
}

/// Represents color values that can be used for bold. See `bold-color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoldColor {
    Color(Color),
    Bright,
}

impl BoldColor {
    /// Convert to the terminal-native BoldColor type.
    pub fn to_terminal(self) -> crate::terminal::style::BoldColor {
        match self {
            BoldColor::Color(col) => crate::terminal::style::BoldColor::Color(col.to_terminal_rgb()),
            BoldColor::Bright => crate::terminal::style::BoldColor::Bright,
        }
    }

    pub fn parse_cli(input_: Option<&str>) -> Result<BoldColor, Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        if input == "bright" {
            return Ok(BoldColor::Bright);
        }
        Ok(BoldColor::Color(Color::parse_cli(Some(input))?))
    }
}

impl ParseCli for BoldColor {
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        BoldColor::parse_cli(input)
    }
}

impl FormatEntry for BoldColor {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        match self {
            BoldColor::Color(c) => c.format_entry(f),
            BoldColor::Bright => f.format_entry("bright"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ColorList {
    pub colors: Vec<Color>,
    pub colors_c: Vec<ColorC>,
}

/// ghostty_config_color_list_s (Zig `ColorList.C`)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ColorListC {
    pub colors: *const ColorC,
    pub len: usize,
}

impl ColorList {
    pub fn cval(&self) -> ColorListC {
        ColorListC {
            colors: self.colors_c.as_ptr(),
            len: self.colors_c.len(),
        }
    }

    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(&self, other: &ColorList) -> bool {
        let items_a = &self.colors;
        let items_b = &other.colors;
        if items_a.len() != items_b.len() {
            return false;
        }
        items_a.iter().zip(items_b).all(|(a, b)| a.equal(*b))
    }
}

impl PartialEq for ColorList {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for ColorList {
    fn parse_cli(&mut self, input_: Option<&str>) -> Result<(), Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        if input.is_empty() {
            return Err(Error::ValueRequired);
        }

        // Always reset on parse
        *self = ColorList::default();

        // Split the input by commas and parse each color
        let mut count: usize = 0;
        for raw in input.split(',').filter(|s| !s.is_empty()) {
            count += 1;
            if count > 64 {
                return Err(Error::InvalidValue);
            }

            // Trim whitespace from each color value
            let trimmed = args::trim(raw, " \t");
            let color = Color::parse_cli(Some(trimmed))?;
            self.colors.push(color);
            self.colors_c.push(color.cval());
        }

        // If no colors were parsed, we need to return an error
        if self.colors.is_empty() {
            return Err(Error::InvalidValue);
        }

        debug_assert_eq!(self.colors.len(), self.colors_c.len());
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for ColorList {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        // If no items, we want to render an empty field.
        if self.colors.is_empty() {
            return f.format_entry(&());
        }

        // Build up the value of our config. Our buffer size should be
        // sized to contain all possible maximum values.
        let mut writer = String::new();
        for (i, color) in self.colors.iter().enumerate() {
            if i != 0 {
                writer.push(',');
            }
            writer.push_str(&color.format_buf());
        }
        if writer.len() > 1024 {
            return Err(formatterpkg::Error::OutOfMemory);
        }

        f.format_entry(writer.as_str())
    }
}

/// Palette is the 256 color palette for 256-color mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The actual value that is updated as we parse.
    pub value: terminal_color::Palette,

    /// Keep track of which indexes were manually set by the user.
    pub mask: terminal_color::PaletteMask,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            value: terminal_color::DEFAULT,
            mask: terminal_color::PaletteMask::EMPTY,
        }
    }
}

/// ghostty_config_palette_s (Zig `Palette.C`)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PaletteC {
    pub colors: [ColorC; 256],
}

impl Palette {
    pub fn cval(&self) -> PaletteC {
        let mut result = PaletteC {
            colors: [ColorC::default(); 256],
        };
        for (i, color) in self.value.iter().enumerate() {
            result.colors[i] = ColorC {
                r: color.r,
                g: color.g,
                b: color.b,
            };
        }

        result
    }

    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(&self, other: &Palette) -> bool {
        self == other
    }
}

impl ParseCli for Palette {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let value = input.ok_or(Error::ValueRequired)?;
        let entry = terminal_color::parse_palette_entry(value.as_bytes()).map_err(|err| match err {
            terminal_color::PaletteEntryError::Overflow => Error::Other("Overflow"),
            terminal_color::PaletteEntryError::InvalidFormat => Error::InvalidValue,
        })?;
        self.value[entry.index as usize] = entry.color;
        self.mask.set(entry.index as usize);
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for Palette {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        for (k, v) in self.value.iter().enumerate() {
            f.format_entry(format!("{}=#{:02x}{:02x}{:02x}", k, v.r, v.g, v.b).as_str())?;
        }
        Ok(())
    }
}

/// RepeatableString is a string value that can be repeated to accumulate
/// a list of strings. This isn't called "StringList" because I find that
/// sometimes leads to confusion that it _accepts_ a list such as
/// comma-separated values.
#[derive(Debug, Default)]
pub struct RepeatableString {
    // Allocator for the list is the arena for the parent config.
    pub list: Vec<StringZ>,

    // If true, then the next value will clear the list and start over
    // rather than append. This is a bit of a hack but is here to make
    // the font-family set of configurations work with CLI parsing.
    pub overwrite_next: bool,
}

impl RepeatableString {
    /// The number of items in the list
    pub fn count(&self) -> usize {
        self.list.len()
    }

    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(&self, other: &RepeatableString) -> bool {
        let items_a = &self.list;
        let items_b = &other.list;
        if items_a.len() != items_b.len() {
            return false;
        }
        items_a.iter().zip(items_b).all(|(a, b)| a == b)
    }
}

impl Clone for RepeatableString {
    /// Deep copy of the struct. Required by Config.
    fn clone(&self) -> Self {
        // Copy the list and all the strings in the list.
        RepeatableString {
            list: self.list.clone(),
            overwrite_next: false,
        }
    }
}

impl PartialEq for RepeatableString {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for RepeatableString {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let value = input.ok_or(Error::ValueRequired)?;

        // Empty value resets the list
        if value.is_empty() {
            self.list.clear();
            return Ok(());
        }

        // If we're overwriting then we clear before appending
        if self.overwrite_next {
            self.list.clear();
            self.overwrite_next = false;
        }

        self.list.push(StringZ::from(value));
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for RepeatableString {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        // If no items, we want to render an empty field.
        if self.list.is_empty() {
            return f.format_entry(&());
        }

        for value in &self.list {
            f.format_entry(value.as_str())?;
        }
        Ok(())
    }
}

/// SelectionWordChars stores the parsed codepoints for word boundary
/// characters used during text selection. The string is parsed once
/// during configuration and stored as u21 codepoints for efficient
/// lookup during selection operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionWordChars {
    /// The parsed codepoints. Always includes null (U+0000) at index 0.
    pub codepoints: Vec<u32>,
}

impl Default for SelectionWordChars {
    fn default() -> Self {
        SelectionWordChars {
            codepoints: selection_codepoints::DEFAULT_WORD_BOUNDARIES.to_vec(),
        }
    }
}

impl SelectionWordChars {
    /// Parse from raw bytes (Zig's `[]const u8` input, which need not be
    /// valid UTF-8).
    pub fn parse_cli_bytes(&mut self, input: Option<&[u8]>) -> Result<(), Error> {
        let value = input.ok_or(Error::ValueRequired)?;

        // Parse string with Zig escape sequence support into codepoints
        let mut list: Vec<u32> = Vec::new();

        // Always include null as first boundary
        list.push(0);

        let mut it = string::codepoint_iterator(value);
        while let Some(codepoint) = it.next().map_err(|_| Error::InvalidValue)? {
            list.push(codepoint);
        }

        self.codepoints = list;
        Ok(())
    }

    /// Compare if two values are equal. Required by Config.
    pub fn equal(&self, other: &SelectionWordChars) -> bool {
        self.codepoints == other.codepoints
    }
}

impl ParseCli for SelectionWordChars {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        self.parse_cli_bytes(input.map(str::as_bytes))
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for SelectionWordChars {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        // Convert codepoints back to UTF-8 string for display
        const BUF_LEN: usize = 4096;
        let mut buf = String::new();

        // Skip the null character at index 0
        for &codepoint in self.codepoints.iter().skip(1) {
            let Some(c) = char::from_u32(codepoint) else {
                continue;
            };
            if buf.len() + c.len_utf8() > BUF_LEN {
                break;
            }
            buf.push(c);
        }

        f.format_entry(buf.as_str())
    }
}

/// FontVariation is a repeatable configuration value that sets a single
/// font variation value. Font variations are configurations for what
/// are often called "variable fonts." The font files usually end in
/// "-VF.ttf."
///
/// The value for this is in the format of `id=value` where `id` is the
/// 4-character font variation axis identifier and `value` is the
/// floating point value for that axis. For more details on font variations
/// see the MDN font-variation-settings documentation since this copies that
/// behavior almost exactly:
///
/// https://developer.mozilla.org/en-US/docs/Web/CSS/font-variation-settings
#[derive(Debug, Clone, Default)]
pub struct RepeatableFontVariation {
    // Allocator for the list is the arena for the parent config.
    pub list: Vec<Variation>,
}

impl RepeatableFontVariation {
    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(&self, other: &RepeatableFontVariation) -> bool {
        let items_a = &self.list;
        let items_b = &other.list;
        if items_a.len() != items_b.len() {
            return false;
        }
        items_a.iter().zip(items_b).all(|(a, b)| a == b)
    }
}

impl PartialEq for RepeatableFontVariation {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for RepeatableFontVariation {
    fn parse_cli(&mut self, input_: Option<&str>) -> Result<(), Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        let eql_idx = input.find('=').ok_or(Error::InvalidValue)?;
        let whitespace = " \t";
        let key = args::trim(&input[..eql_idx], whitespace);
        let value = args::trim(&input[eql_idx + 1..], whitespace);
        if key.len() != 4 {
            return Err(Error::InvalidValue);
        }
        let id: &[u8; 4] = key.as_bytes().try_into().expect("length checked");
        self.list.push(Variation {
            id: VariationId::init(id),
            value: args::parse_float(value).map_err(|_| Error::InvalidValue)?,
        });
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for RepeatableFontVariation {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        if self.list.is_empty() {
            return f.format_entry(&());
        }

        for value in &self.list {
            let id = value.id.str();
            let s = format!("{}={}", String::from_utf8_lossy(&id), value.value);
            if s.len() > 128 {
                return Err(formatterpkg::Error::OutOfMemory);
            }
            f.format_entry(s.as_str())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct RepeatableCodepointMap {
    pub map: CodepointMap,
}

impl RepeatableCodepointMap {
    /// Compare if two of our value are equal. Required by Config.
    pub fn equal(&self, other: &RepeatableCodepointMap) -> bool {
        if self.map.len() != other.map.len() {
            return false;
        }
        (0..self.map.len()).all(|i| self.map.entry(i) == other.map.entry(i))
    }
}

impl PartialEq for RepeatableCodepointMap {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for RepeatableCodepointMap {
    fn parse_cli(&mut self, input_: Option<&str>) -> Result<(), Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        let eql_idx = input.find('=').ok_or(Error::InvalidValue)?;
        let whitespace = " \t";
        let key = args::trim(&input[..eql_idx], whitespace);
        let value = args::trim(&input[eql_idx + 1..], whitespace);
        let value_z = value.to_owned();

        let mut p = UnicodeRangeParser::new(key.as_bytes());
        while let Some(range) = p.next()? {
            self.map.add(CodepointMapEntry {
                range,
                descriptor: Descriptor {
                    family: Some(value_z.clone()),
                    monospace: false, // we allow any font
                    ..Default::default()
                },
            });
        }
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for RepeatableCodepointMap {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        if self.map.is_empty() {
            return f.format_entry(&());
        }

        for (range, descriptor) in self.map.ranges().iter().zip(self.map.descriptors()) {
            let range = *range;
            let family = descriptor.family.as_deref().unwrap_or("");
            let s = if range[0] == range[1] {
                format!("U+{:04X}={}", range[0], family)
            } else {
                format!("U+{:04X}-U+{:04X}={}", range[0], range[1], family)
            };
            if s.len() > 1024 {
                return Err(formatterpkg::Error::OutOfMemory);
            }
            f.format_entry(s.as_str())?;
        }
        Ok(())
    }
}

/// Parses the list of Unicode codepoint ranges. Valid syntax:
///
///   "" (empty returns null)
///   U+1234
///   U+1234-5678
///   U+1234,U+5678
///   U+1234-5678,U+5678
///   U+1234,U+5678-U+9ABC
///
/// etc.
///
/// (Zig: `RepeatableCodepointMap.UnicodeRangeParser`.)
pub struct UnicodeRangeParser<'a> {
    pub input: &'a [u8],
    pub i: usize,
}

impl<'a> UnicodeRangeParser<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        UnicodeRangeParser { input, i: 0 }
    }

    pub fn next(&mut self) -> Result<Option<[u32; 2]>, Error> {
        // Once we're EOF then we're done without an error.
        if self.eof() {
            return Ok(None);
        }

        // One codepoint no matter what
        let start = self.parse_codepoint()?;
        if self.eof() {
            return Ok(Some([start, start]));
        }

        // We're allowed to have any whitespace here
        self.consume_whitespace();

        // Otherwise we expect either a range or a comma
        match self.input[self.i] {
            // Comma means we have another codepoint but in a different
            // range so we return our current codepoint.
            b',' => {
                self.advance();
                self.consume_whitespace();
                if self.eof() {
                    return Err(Error::InvalidValue);
                }
                Ok(Some([start, start]))
            }

            // Hyphen means we have a range.
            b'-' => {
                self.advance();
                self.consume_whitespace();
                if self.eof() {
                    return Err(Error::InvalidValue);
                }
                let end = self.parse_codepoint()?;
                self.consume_whitespace();
                if !self.eof() && self.input[self.i] != b',' {
                    return Err(Error::InvalidValue);
                }
                self.advance();
                self.consume_whitespace();
                if start > end {
                    return Err(Error::InvalidValue);
                }
                Ok(Some([start, end]))
            }

            _ => Err(Error::InvalidValue),
        }
    }

    fn consume_whitespace(&mut self) {
        while !self.eof() {
            match self.input[self.i] {
                b' ' | b'\t' => self.advance(),
                _ => return,
            }
        }
    }

    fn parse_codepoint(&mut self) -> Result<u32, Error> {
        if self.input[self.i] != b'U' {
            return Err(Error::InvalidValue);
        }
        self.advance();
        if self.eof() {
            return Err(Error::InvalidValue);
        }
        if self.input[self.i] != b'+' {
            return Err(Error::InvalidValue);
        }
        self.advance();
        if self.eof() {
            return Err(Error::InvalidValue);
        }

        let start_i = self.i;
        loop {
            let current = self.input[self.i];
            let is_hex = current.is_ascii_hexdigit();
            if !is_hex {
                break;
            }

            // Advance but break on EOF
            self.advance();
            if self.eof() {
                break;
            }
        }

        // If we didn't consume a single character, we have an error.
        if start_i == self.i {
            return Err(Error::InvalidValue);
        }

        let digits = std::str::from_utf8(&self.input[start_i..self.i]).map_err(|_| Error::InvalidValue)?;
        parse_u21(digits, 16)
    }

    fn advance(&mut self) {
        self.i += 1;
    }

    fn eof(&self) -> bool {
        self.i >= self.input.len()
    }
}

/// Zig `std.fmt.parseInt(u21, s, base)`.
fn parse_u21(s: &str, base: u8) -> Result<u32, Error> {
    let v = args::parse_int::<u32>(s, base).map_err(|_| Error::InvalidValue)?;
    if v > 0x1F_FFFF {
        return Err(Error::InvalidValue);
    }
    Ok(v)
}

/// See "clipboard-codepoint-map" for documentation.
#[derive(Debug, Clone, Default)]
pub struct RepeatableClipboardCodepointMap {
    pub map: ClipboardCodepointMap,
}

impl RepeatableClipboardCodepointMap {
    /// Compare if two of our value are equal. Required by Config.
    pub fn equal(&self, other: &RepeatableClipboardCodepointMap) -> bool {
        let items_a = &self.map.list;
        let items_b = &other.map.list;
        if items_a.len() != items_b.len() {
            return false;
        }
        for (a, b) in items_a.iter().zip(items_b) {
            if a.range != b.range {
                return false;
            }
            match (&a.replacement, &b.replacement) {
                (
                    clipboard_codepoint_map::Replacement::Codepoint(cp_a),
                    clipboard_codepoint_map::Replacement::Codepoint(cp_b),
                ) => {
                    if cp_a != cp_b {
                        return false;
                    }
                }
                (
                    clipboard_codepoint_map::Replacement::String(str_a),
                    clipboard_codepoint_map::Replacement::String(str_b),
                ) => {
                    if str_a != str_b {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

impl PartialEq for RepeatableClipboardCodepointMap {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for RepeatableClipboardCodepointMap {
    fn parse_cli(&mut self, input_: Option<&str>) -> Result<(), Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        let eql_idx = input.find('=').ok_or(Error::InvalidValue)?;
        let whitespace = " \t";
        let key = args::trim(&input[..eql_idx], whitespace);
        let value = args::trim(&input[eql_idx + 1..], whitespace);

        // Parse the replacement value - either a codepoint or string
        let replacement = if let Some(cp_str) = value.strip_prefix("U+") {
            // Parse as codepoint
            let cp = parse_u21(cp_str, 16)?;
            clipboard_codepoint_map::Replacement::Codepoint(cp)
        } else {
            // Parse as UTF-8 string - validate it's valid UTF-8 (a Rust
            // `&str` always is).
            clipboard_codepoint_map::Replacement::String(value.to_owned())
        };

        let mut p = UnicodeRangeParser::new(key.as_bytes());
        while let Some(range) = p.next()? {
            self.map.add(clipboard_codepoint_map::Entry {
                range,
                replacement: replacement.clone(),
            });
        }
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for RepeatableClipboardCodepointMap {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        if self.map.list.is_empty() {
            return f.format_entry(&());
        }

        for entry in &self.map.list {
            let range = entry.range;
            let value_str = match &entry.replacement {
                clipboard_codepoint_map::Replacement::Codepoint(cp) => format!("U+{:04X}", cp),
                clipboard_codepoint_map::Replacement::String(s) => s.clone(),
            };

            let s = if range[0] == range[1] {
                format!("U+{:04X}={}", range[0], value_str)
            } else {
                format!("U+{:04X}-U+{:04X}={}", range[0], range[1], value_str)
            };
            if s.len() > 1024 {
                return Err(formatterpkg::Error::OutOfMemory);
            }
            f.format_entry(s.as_str())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontStyle {
    /// Use the default font style that font discovery finds.
    Default,

    /// Disable this font style completely. This will fall back to using
    /// the regular font when this style is encountered.
    False,

    /// A specific named font style to use for this style.
    Name(StringZ),
}

impl FontStyle {
    /// Returns the string name value that can be used with a font
    /// descriptor.
    pub fn name_value(&self) -> Option<&StringZ> {
        match self {
            FontStyle::Default | FontStyle::False => None,
            FontStyle::Name(name) => Some(name),
        }
    }
}

impl ParseCli for FontStyle {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let value = input.ok_or(Error::ValueRequired)?;

        if value == "default" {
            *self = FontStyle::Default;
            return Ok(());
        }

        if value == "false" {
            *self = FontStyle::False;
            return Ok(());
        }

        *self = FontStyle::Name(StringZ::from(value));
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        let mut v = FontStyle::Default;
        v.parse_cli(input)?;
        Ok(v)
    }
}

impl FormatEntry for FontStyle {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        match self {
            FontStyle::Default => f.format_entry("default"),
            FontStyle::False => f.format_entry("false"),
            FontStyle::Name(name) => f.format_entry(name),
        }
    }
}

cli_flags! {
    /// See `font-synthetic-style` for documentation.
    pub struct FontSyntheticStyle {
        pub bold: bool = true,
        pub italic: bool = true,
        pub bold_italic as "bold-italic": bool = true,
    }
}

cli_flags! {
    /// See "font-shaping-break" for documentation
    pub struct FontShapingBreak {
        pub cursor: bool = true,
    }
}

/// See "link" for documentation.
#[derive(Debug, Default)]
pub struct RepeatableLink {
    pub links: Vec<crate::input::link::Link>,
}

impl Clone for RepeatableLink {
    /// Deep copy of the struct. Required by Config.
    fn clone(&self) -> Self {
        RepeatableLink {
            links: self.links.iter().map(|item| item.clone_link()).collect(),
        }
    }
}

impl RepeatableLink {
    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(&self, other: &RepeatableLink) -> bool {
        let items_a = &self.links;
        let items_b = &other.links;
        if items_a.len() != items_b.len() {
            return false;
        }
        items_a.iter().zip(items_b).all(|(a, b)| a.equal(b))
    }
}

impl PartialEq for RepeatableLink {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for RepeatableLink {
    fn parse_cli(&mut self, input_: Option<&str>) -> Result<(), Error> {
        let _ = input_;
        Err(Error::Other("NotImplemented"))
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for RepeatableLink {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        // This currently can't be set so we don't format anything.
        let _ = f;
        Ok(())
    }
}

cli_enum! {
    /// Options for copy on select behavior.
    pub enum CopyOnSelect {
        /// Disables copy on select entirely. This is the default on macOS.
        None => "none",

        /// Copy on select is enabled, but goes to the selection clipboard.
        /// This is not supported on platforms such as macOS. This is the default
        /// on Linux.
        Primary => "primary",

        /// Copy on select is enabled and goes to the system clipboard.
        Clipboard => "clipboard",

        /// Copy on select is enabled and goes to both the system clipboard
        /// and the selection clipboard (for Linux).
        Both => "both",
    }
}

cli_enum! {
    /// Options for right-click actions.
    pub enum RightClickAction {
        /// No action is taken on right-click.
        Ignore => "ignore",

        /// Pastes from the system clipboard.
        Paste => "paste",

        /// Copies the selected text to the system clipboard.
        Copy => "copy",

        /// Copies the selected text to the system clipboard and
        /// pastes the clipboard if no text is selected.
        CopyOrPaste => "copy-or-paste",

        /// Shows a context menu with options.
        ContextMenu => "context-menu",
    }
}

cli_enum! {
    /// Options for middle-click actions.
    pub enum MiddleClickAction {
        /// Paste from the selection clipboard.
        PrimaryPaste => "primary-paste",
        /// Paste from the standard clipboard.
        ClipboardPaste => "clipboard-paste",

        /// No action is taken on middle click.
        Ignore => "ignore",
    }
}

cli_enum! {
    /// Shell integration values
    pub enum ShellIntegration {
        None => "none",
        Detect => "detect",
        Bash => "bash",
        Elvish => "elvish",
        Fish => "fish",
        Nushell => "nushell",
        Zsh => "zsh",
    }
}

cli_flags! {
    /// Shell integration features
    pub struct ShellIntegrationFeatures {
        pub cursor: bool = true,
        pub sudo: bool = false,
        pub title: bool = true,
        pub ssh_env as "ssh-env": bool = false,
        pub ssh_terminfo as "ssh-terminfo": bool = false,
        pub path: bool = true,
    }
}

cli_flags! {
    /// See vt-xt-checksum-extension
    pub struct XtChecksumExtension {
        pub negate: bool = true,
        pub attributes: bool = true,
        pub trim: bool = true,
        pub undrawn: bool = false,
        pub full: bool = false,
    }
}

cli_flags! {
    pub struct SplitPreserveZoom {
        pub navigation: bool = false,
    }
}

cli_enum! {
    /// OSC 4, 10, 11, and 12 default color reporting format.
    pub enum OSCColorReportFormat {
        None => "none",
        Bit8 => "8-bit",
        Bit16 => "16-bit",
    }
}

cli_enum! {
    /// The default window theme.
    pub enum WindowTheme {
        Auto => "auto",
        System => "system",
        Light => "light",
        Dark => "dark",
        Ghostty => "ghostty",
    }
}

cli_enum! {
    /// See window-colorspace
    pub enum WindowColorspace {
        Srgb => "srgb",
        DisplayP3 => "display-p3",
    }
}

cli_enum! {
    /// See macos-window-buttons
    pub enum MacWindowButtons {
        Visible => "visible",
        Hidden => "hidden",
    }
}

cli_enum! {
    /// See macos-titlebar-style
    pub enum MacTitlebarStyle {
        Native => "native",
        Transparent => "transparent",
        Tabs => "tabs",
        Hidden => "hidden",
    }
}

cli_enum! {
    /// See macos-titlebar-proxy-icon
    pub enum MacTitlebarProxyIcon {
        Visible => "visible",
        Hidden => "hidden",
    }
}

cli_enum! {
    /// See macos-hidden
    pub enum MacHidden {
        Never => "never",
        Always => "always",
    }
}

cli_enum! {
    /// See macos-icon
    ///
    /// Note: future versions of Ghostty can support a custom icon with
    /// path by changing this to a tagged union, which doesn't change our
    /// format at all.
    pub enum MacAppIcon {
        Official => "official",
        Blueprint => "blueprint",
        Chalkboard => "chalkboard",
        Microchip => "microchip",
        Glass => "glass",
        Holographic => "holographic",
        Paper => "paper",
        Retro => "retro",
        Xray => "xray",
        Custom => "custom",
        CustomStyle => "custom-style",
    }
}

cli_enum! {
    /// See macos-icon-frame
    pub enum MacAppIconFrame {
        Aluminum => "aluminum",
        Beige => "beige",
        Plastic => "plastic",
        Chrome => "chrome",
    }
}

cli_enum! {
    /// See macos-shortcuts
    pub enum MacShortcuts {
        Allow => "allow",
        Deny => "deny",
        Ask => "ask",
    }
}

cli_enum! {
    /// See gtk-single-instance
    pub enum GtkSingleInstance {
        False => "false",
        True => "true",
        Detect => "detect",
    }
}

impl GtkSingleInstance {
    pub const DEFAULT: GtkSingleInstance = GtkSingleInstance::Detect;
}

cli_enum! {
    /// See gtk-tabs-location
    pub enum GtkTabsLocation {
        Top => "top",
        Bottom => "bottom",
    }
}

cli_enum! {
    /// See gtk-toolbar-style
    pub enum GtkToolbarStyle {
        Flat => "flat",
        Raised => "raised",
        RaisedBorder => "raised-border",
    }
}

cli_enum! {
    /// See gtk-titlebar-style
    ///
    /// (Zig also defines `getGObjectType` to register the enum as the
    /// GObject enum type "GhosttyGtkTitlebarStyle" in GTK builds; that
    /// belongs to the GTK apprt port.)
    #[repr(C)]
    pub enum GtkTitlebarStyle {
        Native => "native",
        Tabs => "tabs",
    }
}

cli_flags! {
    /// See app-notifications
    pub struct AppNotifications {
        pub clipboard_copy as "clipboard-copy": bool = true,
        pub config_reload as "config-reload": bool = true,
    }
}

cli_flags! {
    /// See bell-features
    pub struct BellFeatures {
        pub system: bool = false,
        pub audio: bool = false,
        pub attention: bool = true,
        pub title: bool = true,
        pub border: bool = false,
    }
}

cli_enum! {
    /// See mouse-shift-capture
    pub enum MouseShiftCapture {
        False => "false",
        True => "true",
        Always => "always",
        Never => "never",
    }
}

cli_struct! {
    /// See mouse-scroll-multiplier
    #[derive(Debug, Clone, Copy)]
    pub struct MouseScrollMultiplier {
        pub precision: f64 = 1.0,
        pub discrete: f64 = 3.0,
    }
}

impl MouseScrollMultiplier {
    pub const DEFAULT: MouseScrollMultiplier = MouseScrollMultiplier {
        precision: 1.0,
        discrete: 3.0,
    };

    /// Compare if two of our value are equal. Required by Config.
    pub fn equal(&self, other: &MouseScrollMultiplier) -> bool {
        self.precision == other.precision && self.discrete == other.discrete
    }
}

impl PartialEq for MouseScrollMultiplier {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for MouseScrollMultiplier {
    fn parse_cli(&mut self, input_: Option<&str>) -> Result<(), Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        *self = match args::parse_auto_struct::<MouseScrollMultiplier>(input, Some(self)) {
            Ok(v) => v,
            Err(Error::InvalidValue) => {
                let v = args::parse_float(input).map_err(|_| Error::InvalidValue)?;
                MouseScrollMultiplier {
                    precision: v,
                    discrete: v,
                }
            }
            Err(err) => return Err(err),
        };
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for MouseScrollMultiplier {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        let s = format!("precision:{},discrete:{}", self.precision, self.discrete);
        f.format_entry(s.as_str())
    }
}

cli_enum! {
    /// How to treat requests to write to or read from the clipboard
    pub enum ClipboardAccess {
        Allow => "allow",
        Deny => "deny",
        Ask => "ask",
    }
}

cli_enum! {
    /// See window-save-state
    pub enum WindowSaveState {
        Default => "default",
        Never => "never",
        Always => "always",
    }
}

cli_enum! {
    /// See window-new-tab-position
    pub enum WindowNewTabPosition {
        Current => "current",
        End => "end",
    }
}

cli_enum! {
    /// See macos-dock-drop-behavior
    pub enum MacOSDockDropBehavior {
        NewTab => "new-tab",
        NewWindow => "new-window",
    }
}

cli_enum! {
    /// See window-show-tab-bar
    pub enum WindowShowTabBar {
        Always => "always",
        Auto => "auto",
        Never => "never",
    }
}

cli_enum! {
    /// See drag-handle
    pub enum DragHandle {
        Always => "always",
        Auto => "auto",
        Never => "never",
    }
}

cli_enum! {
    /// See resize-overlay
    pub enum ResizeOverlay {
        Always => "always",
        Never => "never",
        AfterFirst => "after-first",
    }
}

cli_enum! {
    /// See resize-overlay-position
    pub enum ResizeOverlayPosition {
        Center => "center",
        TopLeft => "top-left",
        TopCenter => "top-center",
        TopRight => "top-right",
        BottomLeft => "bottom-left",
        BottomCenter => "bottom-center",
        BottomRight => "bottom-right",
    }
}

cli_enum! {
    /// See quick-terminal-position
    pub enum QuickTerminalPosition {
        Top => "top",
        Bottom => "bottom",
        Left => "left",
        Right => "right",
        Center => "center",
    }
}

cli_enum! {
    /// See quick-terminal-layer
    pub enum QuickTerminalLayer {
        Overlay => "overlay",
        Top => "top",
        Bottom => "bottom",
        Background => "background",
    }
}

/// See quick-terminal-size
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct QuickTerminalSize {
    pub primary: Option<QuickTerminalSizeSize>,
    pub secondary: Option<QuickTerminalSizeSize>,
}

/// Zig `QuickTerminalSize.Size`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QuickTerminalSizeSize {
    Percentage(f32),
    Pixels(u32),
}

impl QuickTerminalSizeSize {
    pub fn to_pixels(self, parent_dimensions: u32) -> u32 {
        match self {
            QuickTerminalSizeSize::Percentage(v) => {
                let dim = parent_dimensions as f32;
                (v / 100.0 * dim) as u32
            }
            QuickTerminalSizeSize::Pixels(v) => v,
        }
    }

    pub fn parse(input: &str) -> Result<QuickTerminalSizeSize, Error> {
        if input.is_empty() {
            return Err(Error::ValueRequired);
        }

        if let Some(px) = input.strip_suffix("px") {
            return Ok(QuickTerminalSizeSize::Pixels(
                args::parse_int::<u32>(px, 10).map_err(|_| Error::InvalidValue)?,
            ));
        }

        if let Some(pct) = input.strip_suffix('%') {
            let percentage = args::parse_float(pct).map_err(|_| Error::InvalidValue)? as f32;

            if percentage < 0.0 {
                return Err(Error::InvalidValue);
            }
            return Ok(QuickTerminalSizeSize::Percentage(percentage));
        }

        Err(Error::Other("MissingUnit"))
    }

    fn format(self, writer: &mut String) {
        match self {
            QuickTerminalSizeSize::Percentage(v) => {
                let _ = write!(writer, "{}%", v);
            }
            QuickTerminalSizeSize::Pixels(v) => {
                let _ = write!(writer, "{}px", v);
            }
        }
    }
}

/// Zig `QuickTerminalSize.Dimensions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuickTerminalSizeDimensions {
    pub width: u32,
    pub height: u32,
}

/// C API structure for QuickTerminalSize (Zig `QuickTerminalSize.C`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct QuickTerminalSizeC {
    pub primary: QuickTerminalSizeCSize,
    pub secondary: QuickTerminalSizeCSize,
}

/// Zig `QuickTerminalSize.C.Size`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct QuickTerminalSizeCSize {
    pub tag: QuickTerminalSizeCTag,
    pub value: QuickTerminalSizeCValue,
}

/// c_int because it needs to be extern compatible
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickTerminalSizeCTag {
    None,
    Percentage,
    Pixels,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union QuickTerminalSizeCValue {
    pub percentage: f32,
    pub pixels: u32,
}

impl QuickTerminalSizeCSize {
    pub const NONE: QuickTerminalSizeCSize = QuickTerminalSizeCSize {
        tag: QuickTerminalSizeCTag::None,
        value: QuickTerminalSizeCValue { pixels: 0 },
    };

    pub fn percentage(v: f32) -> QuickTerminalSizeCSize {
        QuickTerminalSizeCSize {
            tag: QuickTerminalSizeCTag::Percentage,
            value: QuickTerminalSizeCValue { percentage: v },
        }
    }

    pub fn pixels(v: u32) -> QuickTerminalSizeCSize {
        QuickTerminalSizeCSize {
            tag: QuickTerminalSizeCTag::Pixels,
            value: QuickTerminalSizeCValue { pixels: v },
        }
    }

    fn from_size(size: Option<QuickTerminalSizeSize>) -> QuickTerminalSizeCSize {
        match size {
            Some(QuickTerminalSizeSize::Percentage(v)) => QuickTerminalSizeCSize::percentage(v),
            Some(QuickTerminalSizeSize::Pixels(v)) => QuickTerminalSizeCSize::pixels(v),
            None => QuickTerminalSizeCSize::NONE,
        }
    }
}

impl QuickTerminalSize {
    pub fn cval(&self) -> QuickTerminalSizeC {
        QuickTerminalSizeC {
            primary: QuickTerminalSizeCSize::from_size(self.primary),
            secondary: QuickTerminalSizeCSize::from_size(self.secondary),
        }
    }

    pub fn calculate(
        &self,
        position: QuickTerminalPosition,
        dims: QuickTerminalSizeDimensions,
    ) -> QuickTerminalSizeDimensions {
        let px = |v: Option<QuickTerminalSizeSize>, dim: u32, default: u32| match v {
            Some(v) => v.to_pixels(dim),
            None => default,
        };
        match position {
            QuickTerminalPosition::Left | QuickTerminalPosition::Right => QuickTerminalSizeDimensions {
                width: px(self.primary, dims.width, 400),
                height: px(self.secondary, dims.height, dims.height),
            },
            QuickTerminalPosition::Top | QuickTerminalPosition::Bottom => QuickTerminalSizeDimensions {
                width: px(self.secondary, dims.width, dims.width),
                height: px(self.primary, dims.height, 400),
            },
            QuickTerminalPosition::Center => {
                if dims.width >= dims.height {
                    QuickTerminalSizeDimensions {
                        width: px(self.primary, dims.width, 800),
                        height: px(self.secondary, dims.height, 400),
                    }
                } else {
                    QuickTerminalSizeDimensions {
                        width: px(self.secondary, dims.width, 400),
                        height: px(self.primary, dims.height, 800),
                    }
                }
            }
        }
    }
}

impl ParseCli for QuickTerminalSize {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let input_ = input.ok_or(Error::ValueRequired)?;
        let mut it = input_.split(',');

        let primary = args::trim(it.next().ok_or(Error::ValueRequired)?, args::WHITESPACE);
        self.primary = Some(QuickTerminalSizeSize::parse(primary)?);

        self.secondary = match it.next() {
            None => None,
            Some(secondary) => {
                let secondary = args::trim(secondary, args::WHITESPACE);
                Some(QuickTerminalSizeSize::parse(secondary)?)
            }
        };

        if it.next().is_some() {
            return Err(Error::Other("TooManyArguments"));
        }
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }
}

impl FormatEntry for QuickTerminalSize {
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        let Some(primary) = self.primary else {
            return Ok(());
        };

        let mut writer = String::new();
        primary.format(&mut writer);
        if let Some(secondary) = self.secondary {
            writer.push(',');
            secondary.format(&mut writer);
        }

        f.format_entry(writer.as_str())
    }
}

cli_enum! {
    /// See quick-terminal-screen
    pub enum QuickTerminalScreen {
        Main => "main",
        Mouse => "mouse",
        MacosMenuBar => "macos-menu-bar",
    }
}

cli_enum! {
    // See quick-terminal-space-behavior
    pub enum QuickTerminalSpaceBehavior {
        Remain => "remain",
        Move => "move",
    }
}

cli_enum! {
    /// See quick-terminal-keyboard-interactivity
    pub enum QuickTerminalKeyboardInteractivity {
        None => "none",
        OnDemand => "on-demand",
        Exclusive => "exclusive",
    }
}

cli_enum! {
    /// See grapheme-width-method
    pub enum GraphemeWidthMethod {
        Legacy => "legacy",
        Unicode => "unicode",
    }
}

cli_enum! {
    /// See alpha-blending
    pub enum AlphaBlending {
        Native => "native",
        Linear => "linear",
        LinearCorrected => "linear-corrected",
    }
}

impl AlphaBlending {
    pub fn is_linear(self) -> bool {
        match self {
            AlphaBlending::Native => false,
            AlphaBlending::Linear | AlphaBlending::LinearCorrected => true,
        }
    }
}

cli_enum! {
    /// See background-image-position
    pub enum BackgroundImagePosition {
        TopLeft => "top-left",
        TopCenter => "top-center",
        TopRight => "top-right",
        CenterLeft => "center-left",
        CenterCenter => "center-center",
        CenterRight => "center-right",
        BottomLeft => "bottom-left",
        BottomCenter => "bottom-center",
        BottomRight => "bottom-right",
        Center => "center",
    }
}

cli_enum! {
    /// See background-image-fit
    pub enum BackgroundImageFit {
        Contain => "contain",
        Cover => "cover",
        Stretch => "stretch",
        None => "none",
    }
}

cli_flags! {
    /// See freetype-load-flag
    pub struct FreetypeLoadFlags {
        // The defaults here at the time of writing this match the defaults
        // for Freetype itself. Ghostty hasn't made any opinionated changes
        // to these defaults. (Strictly speaking, `light` isn't FreeType's
        // own default, but appears to be the effective default with most
        // Fontconfig-aware software using FreeType, so until Ghostty
        // implements Fontconfig support we default to `light`.)
        pub hinting: bool = true,
        pub force_autohint as "force-autohint": bool = false,
        pub monochrome: bool = false,
        pub autohint: bool = true,
        pub light: bool = true,
    }
}

cli_enum! {
    /// See linux-cgroup
    pub enum LinuxCgroup {
        Never => "never",
        Always => "always",
        SingleInstance => "single-instance",
    }
}

cli_enum! {
    /// See async-backend
    pub enum AsyncBackend {
        Auto => "auto",
        Epoll => "epoll",
        IoUring => "io_uring",
    }
}

cli_enum! {
    /// See auto-updates
    pub enum AutoUpdate {
        Off => "off",
        Check => "check",
        Download => "download",
    }
}

/// See background-blur
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundBlur {
    False,
    True,
    MacosGlassRegular,
    MacosGlassClear,
    Radius(u8),
}

impl BackgroundBlur {
    pub fn enabled(self) -> bool {
        match self {
            BackgroundBlur::False => false,
            BackgroundBlur::True => true,
            BackgroundBlur::Radius(v) => v > 0,

            // We treat these as true because they both imply some blur!
            // This has the effect of making the standard blur happen on
            // Linux.
            BackgroundBlur::MacosGlassRegular | BackgroundBlur::MacosGlassClear => true,
        }
    }

    pub fn cval(self) -> i16 {
        match self {
            BackgroundBlur::False => 0,
            BackgroundBlur::True => 20,
            BackgroundBlur::Radius(v) => v as i16,
            // I hate sentinel values like this but this is only for
            // our macOS application currently. We can switch to a proper
            // tagged union if we ever need to.
            BackgroundBlur::MacosGlassRegular => -1,
            BackgroundBlur::MacosGlassClear => -2,
        }
    }
}

impl ParseCli for BackgroundBlur {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let Some(input_) = input else {
            // Emulate behavior for bools
            *self = BackgroundBlur::True;
            return Ok(());
        };

        // Try to parse normal bools
        if let Ok(b) = args::parse_bool(input_) {
            *self = if b { BackgroundBlur::True } else { BackgroundBlur::False };
            return Ok(());
        }

        // Try to parse enums. We can only parse void types.
        match input_ {
            "false" => {
                *self = BackgroundBlur::False;
                return Ok(());
            }
            "true" => {
                *self = BackgroundBlur::True;
                return Ok(());
            }
            "macos-glass-regular" => {
                *self = BackgroundBlur::MacosGlassRegular;
                return Ok(());
            }
            "macos-glass-clear" => {
                *self = BackgroundBlur::MacosGlassClear;
                return Ok(());
            }
            _ => {}
        }

        *self = BackgroundBlur::Radius(args::parse_int::<u8>(input_, 0).map_err(|_| Error::InvalidValue)?);
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        let mut v = BackgroundBlur::False;
        v.parse_cli(input)?;
        Ok(v)
    }
}

impl FormatEntry for BackgroundBlur {
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        match self {
            BackgroundBlur::False => f.format_entry(&false),
            BackgroundBlur::True => f.format_entry(&true),
            BackgroundBlur::Radius(v) => f.format_entry(v),
            BackgroundBlur::MacosGlassRegular => f.format_entry("macos-glass-regular"),
            BackgroundBlur::MacosGlassClear => f.format_entry("macos-glass-clear"),
        }
    }
}

cli_enum! {
    @custom_parse
    /// See window-decoration
    ///
    /// (Zig also defines `getGObjectType` to register the enum as the
    /// GObject enum type "GhosttyConfigWindowDecoration" in GTK builds;
    /// that belongs to the GTK apprt port.)
    #[repr(C)]
    pub enum WindowDecoration {
        Auto => "auto",
        Client => "client",
        Server => "server",
        None => "none",
    }
}

impl WindowDecoration {
    pub fn parse_cli(input_: Option<&str>) -> Result<WindowDecoration, Error> {
        let Some(input) = input_ else {
            return Ok(WindowDecoration::Auto);
        };

        match args::parse_bool(input) {
            Ok(b) => Ok(if b { WindowDecoration::Auto } else { WindowDecoration::None }),
            Err(_) => WindowDecoration::from_tag(input).ok_or(Error::InvalidValue),
        }
    }
}

impl ParseCli for WindowDecoration {
    const ENUM_VALUES: Option<&'static [&'static str]> = Some(<Self as CliEnum>::FIELD_NAMES);

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        WindowDecoration::parse_cli(input)
    }
}

cli_struct! {
    /// See theme
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Theme {
        pub light: String,
        pub dark: String,
    }
}

impl Theme {
    fn parse(input_: Option<&str>) -> Result<Theme, Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        if input.is_empty() {
            return Err(Error::ValueRequired);
        }

        // If there is a comma, equal sign, or colon, then we assume that
        // we're parsing a light/dark mode theme pair. Note that "=" isn't
        // actually valid for setting a light/dark mode pair but I anticipate
        // it'll be a common typo.
        //
        // On Windows, a colon at index 1 is a drive letter (e.g. C:\...)
        // and should not trigger light/dark pair parsing.
        let has_colon = if cfg!(windows) {
            input.find(':').is_some_and(|idx| idx != 1)
        } else {
            input.contains(':')
        };
        if input.contains(',') || input.contains('=') || has_colon {
            return args::parse_auto_struct::<Theme>(input, None);
        }

        // Trim our value
        let trimmed = args::trim(input, args::WHITESPACE);

        // Set the value to the specified value directly.
        Ok(Theme {
            light: trimmed.to_owned(),
            dark: trimmed.to_owned(),
        })
    }

    /// Expand tilde paths in light/dark theme values.
    pub fn finalize(&mut self) {
        let light = expand_home(&self.light);
        if light != self.light {
            self.light = light.into_owned();
        }

        let dark = expand_home(&self.dark);
        if dark != self.dark {
            self.dark = dark.into_owned();
        }
    }
}

impl ParseCli for Theme {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        *self = Theme::parse(input)?;
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        Theme::parse(input)
    }
}

impl FormatEntry for Theme {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        if self.light == self.dark {
            return f.format_entry(self.light.as_str());
        }

        let s = format!("light:{},dark:{}", self.light, self.dark);
        if s.len() > 4096 {
            return Err(formatterpkg::Error::OutOfMemory);
        }
        f.format_entry(s.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Duration {
    /// Duration in nanoseconds
    pub duration: u64,
}

/// A duration unit (Zig `Duration.units` element).
#[derive(Debug, Clone, Copy)]
pub struct DurationUnit {
    pub name: &'static str,
    pub factor: u64,
}

impl Duration {
    pub const UNITS: [DurationUnit; 10] = [
        // The order is important as the first factor that matches will be the
        // default unit that is used for formatting.
        DurationUnit { name: "y", factor: 365 * NS_PER_DAY },
        DurationUnit { name: "w", factor: NS_PER_WEEK },
        DurationUnit { name: "d", factor: NS_PER_DAY },
        DurationUnit { name: "h", factor: NS_PER_HOUR },
        DurationUnit { name: "m", factor: NS_PER_MIN },
        DurationUnit { name: "s", factor: NS_PER_S },
        DurationUnit { name: "ms", factor: NS_PER_MS },
        DurationUnit { name: "µs", factor: NS_PER_US },
        DurationUnit { name: "us", factor: NS_PER_US },
        DurationUnit { name: "ns", factor: 1 },
    ];

    pub fn equal(self, other: Duration) -> bool {
        self.duration == other.duration
    }

    pub fn round(self, to: u64) -> Duration {
        Duration {
            duration: self.duration / to * to,
        }
    }

    pub fn lte(self, other: Duration) -> bool {
        self.duration <= other.duration
    }

    pub fn parse_cli(input: Option<&str>) -> Result<Duration, Error> {
        let mut remaining: &[u8] = input.ok_or(Error::ValueRequired)?.as_bytes();

        let mut value: Option<u64> = None;
        while !remaining.is_empty() {
            // Skip over whitespace before the number
            while !remaining.is_empty() && is_ascii_whitespace(remaining[0]) {
                remaining = &remaining[1..];
            }

            // There was whitespace at the end, that's OK
            if remaining.is_empty() {
                break;
            }

            // Find the longest number
            let number: u64 = 'number: {
                let mut prev_number: Option<u64> = None;
                let mut prev_remaining: Option<&[u8]> = None;
                for index in 1..remaining.len() + 1 {
                    match parse_unsigned_u64(&remaining[..index]) {
                        Ok(n) => prev_number = Some(n),
                        Err(()) => {
                            if let Some(prev) = prev_remaining {
                                remaining = prev;
                            }
                            break 'number prev_number;
                        }
                    }
                    prev_remaining = Some(&remaining[index..]);
                }
                if let Some(prev) = prev_remaining {
                    remaining = prev;
                }
                prev_number
            }
            .ok_or(Error::InvalidValue)?;

            // A number without a unit is invalid unless the number is
            // exactly zero. In that case, the unit is unambiguous since
            // its all the same.
            if remaining.is_empty() {
                if number == 0 {
                    value = Some(0);
                    break;
                }

                return Err(Error::InvalidValue);
            }

            // Find the longest matching unit. Needs to be the longest matching
            // to distinguish 'm' from 'ms'.
            let factor = {
                let mut prev_factor: Option<u64> = None;
                let mut prev_index: Option<usize> = None;
                for index in 1..remaining.len() + 1 {
                    let next_factor = Self::UNITS
                        .iter()
                        .find(|unit| unit.name.as_bytes() == &remaining[..index])
                        .map(|unit| unit.factor);
                    if let Some(next) = next_factor {
                        prev_factor = Some(next);
                        prev_index = Some(index);
                    }
                }
                if let Some(index) = prev_index {
                    remaining = &remaining[index..];
                }
                prev_factor
            }
            .ok_or(Error::InvalidValue)?;

            // Add our time value to the total. Avoid overflow with saturating math.
            let diff = number.checked_mul(factor).unwrap_or(u64::MAX);
            value = Some(value.unwrap_or(0).saturating_add(diff));
        }

        match value {
            Some(v) => Ok(Duration { duration: v }),
            None => Err(Error::ValueRequired),
        }
    }

    pub fn format(&self, writer: &mut dyn fmt::Write) -> fmt::Result {
        let mut value = self.duration;
        let mut i: usize = 0;
        for unit in Self::UNITS {
            if value >= unit.factor {
                if i > 0 {
                    writer.write_str(" ")?;
                }
                let remainder = value % unit.factor;
                let quotient = (value - remainder) / unit.factor;
                write!(writer, "{}{}", quotient, unit.name)?;
                value = remainder;
                i += 1;
            }
        }
        Ok(())
    }

    pub fn cval(self) -> usize {
        self.as_milliseconds() as usize
    }

    /// Convenience function to convert to milliseconds since many OS and
    /// library timing functions operate on that timescale.
    pub fn as_milliseconds(self) -> std::ffi::c_uint {
        let ms: u64 = self.duration / NS_PER_MS;
        std::ffi::c_uint::try_from(ms).unwrap_or(std::ffi::c_uint::MAX)
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
    }
}

/// Zig `std.ascii.isWhitespace`.
fn is_ascii_whitespace(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Zig `std.fmt.parseUnsigned(u64, buf, 10)`: decimal digits with
/// underscores allowed anywhere except at the start or end; no sign.
fn parse_unsigned_u64(buf: &[u8]) -> Result<u64, ()> {
    if buf.is_empty() || buf[0] == b'_' || buf[buf.len() - 1] == b'_' {
        return Err(());
    }
    let mut acc: u64 = 0;
    for &c in buf {
        if c == b'_' {
            continue;
        }
        if !c.is_ascii_digit() {
            return Err(());
        }
        acc = acc
            .checked_mul(10)
            .and_then(|v| v.checked_add((c - b'0') as u64))
            .ok_or(())?;
    }
    Ok(acc)
}

impl ParseCli for Duration {
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        Duration::parse_cli(input)
    }
}

impl FormatEntry for Duration {
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        let mut buf = String::new();
        self.format(&mut buf)?;
        f.format_entry(buf.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowPadding {
    pub top_left: u32,
    pub bottom_right: u32,
}

impl WindowPadding {
    pub fn equal(self, other: WindowPadding) -> bool {
        self == other
    }

    pub fn parse_cli(input_: Option<&str>) -> Result<WindowPadding, Error> {
        let input = input_.ok_or(Error::ValueRequired)?;
        let whitespace = " \t";

        if let Some(idx) = input.find(',') {
            let input_left = args::trim(&input[..idx], whitespace);
            let input_right = args::trim(&input[idx + 1..], whitespace);
            let left = args::parse_int::<u32>(input_left, 10).map_err(|_| Error::InvalidValue)?;
            let right = args::parse_int::<u32>(input_right, 10).map_err(|_| Error::InvalidValue)?;
            Ok(WindowPadding {
                top_left: left,
                bottom_right: right,
            })
        } else {
            let value = args::parse_int::<u32>(args::trim(input, whitespace), 10)
                .map_err(|_| Error::InvalidValue)?;
            Ok(WindowPadding {
                top_left: value,
                bottom_right: value,
            })
        }
    }
}

impl ParseCli for WindowPadding {
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        WindowPadding::parse_cli(input)
    }
}

impl FormatEntry for WindowPadding {
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        if self.top_left == self.bottom_right {
            f.format_entry(format!("{}", self.top_left).as_str())
        } else {
            f.format_entry(format!("{},{}", self.top_left, self.bottom_right).as_str())
        }
    }
}

cli_enum! {
    /// See scrollbar
    pub enum Scrollbar {
        System => "system",
        Never => "never",
    }
}

cli_flags! {
    /// See scroll-to-bottom
    pub struct ScrollToBottom {
        pub keystroke: bool = true,
        pub output: bool = false,
    }
}

impl ScrollToBottom {
    pub const DEFAULT: ScrollToBottom = ScrollToBottom {
        keystroke: true,
        output: false,
    };
}

cli_enum! {
    /// See notify-on-command-finish
    pub enum NotifyOnCommandFinish {
        Never => "never",
        Unfocused => "unfocused",
        Always => "always",
    }
}

cli_flags! {
    /// See notify-on-command-finish-action
    pub struct NotifyOnCommandFinishAction {
        pub bell: bool = true,
        pub notify: bool = false,
    }
}

// ---------------------------------------------------------------------------
// Keybinds
// ---------------------------------------------------------------------------

use crate::input::binding::{
    self, Action, AdjustSelection, CloseTabMode, CopyToClipboard as CopyToClipboardMode, Flags,
    InspectorMode, NavigateSearch, OpenConfig, SplitDirection, SplitFocusDirection,
    SplitResizeDirection, Trigger, TriggerKey, WriteScreen,
};
use crate::input::key::Key as InputKey;

/// Set the given modifier flags on an empty [`Mods`].
macro_rules! mods {
    ($($f:ident),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut m = Mods::EMPTY;
        $( m.$f = true; )*
        m
    }};
}

/// Zig `builtin.target.os.tag.isDarwin()`.
const IS_DARWIN: bool = cfg!(any(target_os = "macos", target_os = "ios"));

fn unicode(c: char, mods: Mods) -> Trigger {
    Trigger {
        key: TriggerKey::Unicode(c as u32),
        mods,
    }
}

fn physical(key: InputKey, mods: Mods) -> Trigger {
    Trigger {
        key: TriggerKey::Physical(key),
        mods,
    }
}

fn performable(performable: bool) -> Flags {
    Flags {
        performable,
        ..Flags::default()
    }
}

/// Map a binding parse error to the config error (Zig error names).
fn binding_error(err: binding::Error) -> Error {
    match err {
        binding::Error::InvalidFormat => Error::Other("InvalidFormat"),
        binding::Error::InvalidAction => Error::Other("InvalidAction"),
    }
}

/// The most recent binding target for `chain=` additions (Zig
/// `Keybinds.chain_target`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ChainTarget {
    #[default]
    Root,
    Table(String),
}

/// Stores a set of keybinds.
#[derive(Debug, Default)]
pub struct Keybinds {
    pub set: binding::Set,

    /// Defined key tables. The default key table is always the root "set",
    /// which allows all table names to be available without reservation.
    pub tables: indexmap::IndexMap<String, binding::Set>,

    /// The most recent binding target for `chain=` additions.
    ///
    /// This is intentionally tracked at the Keybinds level so that chains can
    /// apply across table boundaries according to parse order.
    pub chain_target: ChainTarget,
}

impl Keybinds {
    pub fn init(&mut self) {
        // We don't clear the memory because it's in the arena and unlikely
        // to be free-able anyways (since arenas can only clear the last
        // allocated value). This isn't a memory leak because the arena
        // will be freed when the config is freed.
        self.set = binding::Set::default();
        self.tables = indexmap::IndexMap::new();
        self.chain_target = ChainTarget::Root;

        let set = &mut self.set;

        // keybinds for opening and reloading config
        set.put(unicode(',', ctrl_or_super(mods!(shift))), Action::ReloadConfig);
        set.put(unicode(',', ctrl_or_super(mods!())), Action::OpenConfig(OpenConfig::DEFAULT));

        {
            set.put(
                physical(InputKey::Copy, mods!()),
                Action::CopyToClipboard(CopyToClipboardMode::Mixed),
            );
            set.put(physical(InputKey::Paste, mods!()), Action::PasteFromClipboard);

            // On non-MacOS desktop envs (Windows, KDE, Gnome, Xfce), ctrl+insert is an
            // alt keybinding for Copy and shift+ins is an alt keybinding for Paste
            //
            // The order of these blocks is important. The *last* added keybind for a given action is
            // what will display in the menu. We want the more typical keybinds after this block to be
            // the standard
            if !IS_DARWIN {
                set.put(
                    physical(InputKey::Insert, mods!(ctrl)),
                    Action::CopyToClipboard(CopyToClipboardMode::Mixed),
                );
                set.put(physical(InputKey::Insert, mods!(shift)), Action::PasteFromClipboard);
            }

            // On macOS we default to super but Linux ctrl+shift since
            // ctrl+c is to kill the process.
            let mods: Mods = if IS_DARWIN { mods!(super_) } else { mods!(ctrl, shift) };

            set.put_flags(
                unicode('c', mods),
                Action::CopyToClipboard(CopyToClipboardMode::Mixed),
                performable(true),
            );
            set.put_flags(unicode('v', mods), Action::PasteFromClipboard, performable(true));
        }

        // Increase font size mapping for keyboards with dedicated plus keys (like german)
        // Note: this order matters below because the C API will only return
        // the last keybinding for a given action. The macOS app uses this to
        // set the expected keybind for the menu.
        set.put(unicode('=', ctrl_or_super(mods!())), Action::IncreaseFontSize(1.0));
        set.put(unicode('+', ctrl_or_super(mods!())), Action::IncreaseFontSize(1.0));

        set.put(unicode('-', ctrl_or_super(mods!())), Action::DecreaseFontSize(1.0));
        set.put(unicode('0', ctrl_or_super(mods!())), Action::ResetFontSize);

        set.put(
            unicode('j', mods!(shift, ctrl, super_)),
            Action::WriteScreenFile(WriteScreen::COPY),
        );

        set.put(
            unicode('j', ctrl_or_super(mods!(shift))),
            Action::WriteScreenFile(WriteScreen::PASTE),
        );

        set.put(
            unicode('j', ctrl_or_super(mods!(shift, alt))),
            Action::WriteScreenFile(WriteScreen::OPEN),
        );

        // Expand Selection
        set.put_flags(
            physical(InputKey::ArrowLeft, mods!(shift)),
            Action::AdjustSelection(AdjustSelection::Left),
            performable(true),
        );
        set.put_flags(
            physical(InputKey::ArrowRight, mods!(shift)),
            Action::AdjustSelection(AdjustSelection::Right),
            performable(true),
        );
        set.put_flags(
            physical(InputKey::ArrowUp, mods!(shift)),
            Action::AdjustSelection(AdjustSelection::Up),
            performable(true),
        );
        set.put_flags(
            physical(InputKey::ArrowDown, mods!(shift)),
            Action::AdjustSelection(AdjustSelection::Down),
            performable(true),
        );
        set.put_flags(
            physical(InputKey::PageUp, mods!(shift)),
            Action::AdjustSelection(AdjustSelection::PageUp),
            performable(true),
        );
        set.put_flags(
            physical(InputKey::PageDown, mods!(shift)),
            Action::AdjustSelection(AdjustSelection::PageDown),
            performable(true),
        );
        set.put_flags(
            physical(InputKey::Home, mods!(shift)),
            Action::AdjustSelection(AdjustSelection::Home),
            performable(true),
        );
        set.put_flags(
            physical(InputKey::End, mods!(shift)),
            Action::AdjustSelection(AdjustSelection::End),
            performable(true),
        );

        // Tabs common to all platforms
        set.put(physical(InputKey::Tab, mods!(ctrl, shift)), Action::PreviousTab);
        set.put(physical(InputKey::Tab, mods!(ctrl)), Action::NextTab);

        // Windowing
        if !IS_DARWIN {
            set.put(unicode('n', mods!(ctrl, shift)), Action::NewWindow);
            set.put(unicode('w', mods!(ctrl, shift)), Action::CloseSurface);
            set.put(unicode('q', mods!(ctrl, shift)), Action::Quit);
            set.put(physical(InputKey::F4, mods!(alt)), Action::CloseWindow);
            set.put(unicode('t', mods!(ctrl, shift)), Action::NewTab);
            set.put(unicode('w', mods!(ctrl, shift)), Action::CloseTab(CloseTabMode::This));
            set.put_flags(
                physical(InputKey::ArrowLeft, mods!(ctrl, shift)),
                Action::PreviousTab,
                performable(true),
            );
            set.put_flags(
                physical(InputKey::ArrowRight, mods!(ctrl, shift)),
                Action::NextTab,
                performable(true),
            );
            set.put_flags(physical(InputKey::PageUp, mods!(ctrl)), Action::PreviousTab, performable(true));
            set.put_flags(physical(InputKey::PageDown, mods!(ctrl)), Action::NextTab, performable(true));
            set.put(unicode('o', mods!(ctrl, shift)), Action::NewSplit(SplitDirection::Right));
            set.put(unicode('e', mods!(ctrl, shift)), Action::NewSplit(SplitDirection::Down));
            set.put_flags(
                unicode('[', mods!(ctrl, super_)),
                Action::GotoSplit(SplitFocusDirection::Previous),
                performable(true),
            );
            set.put_flags(
                unicode(']', mods!(ctrl, super_)),
                Action::GotoSplit(SplitFocusDirection::Next),
                performable(true),
            );
            set.put_flags(
                physical(InputKey::ArrowUp, mods!(ctrl, alt)),
                Action::GotoSplit(SplitFocusDirection::Up),
                performable(true),
            );
            set.put_flags(
                physical(InputKey::ArrowDown, mods!(ctrl, alt)),
                Action::GotoSplit(SplitFocusDirection::Down),
                performable(true),
            );
            set.put_flags(
                physical(InputKey::ArrowLeft, mods!(ctrl, alt)),
                Action::GotoSplit(SplitFocusDirection::Left),
                performable(true),
            );
            set.put_flags(
                physical(InputKey::ArrowRight, mods!(ctrl, alt)),
                Action::GotoSplit(SplitFocusDirection::Right),
                performable(true),
            );

            // Resizing splits
            set.put_flags(
                physical(InputKey::ArrowUp, mods!(super_, ctrl, shift)),
                Action::ResizeSplit((SplitResizeDirection::Up, 10)),
                performable(true),
            );
            set.put_flags(
                physical(InputKey::ArrowDown, mods!(super_, ctrl, shift)),
                Action::ResizeSplit((SplitResizeDirection::Down, 10)),
                performable(true),
            );
            set.put_flags(
                physical(InputKey::ArrowLeft, mods!(super_, ctrl, shift)),
                Action::ResizeSplit((SplitResizeDirection::Left, 10)),
                performable(true),
            );
            set.put_flags(
                physical(InputKey::ArrowRight, mods!(super_, ctrl, shift)),
                Action::ResizeSplit((SplitResizeDirection::Right, 10)),
                performable(true),
            );

            // Viewport scrolling
            set.put(physical(InputKey::Home, mods!(shift)), Action::ScrollToTop);
            set.put(physical(InputKey::End, mods!(shift)), Action::ScrollToBottom);
            set.put(physical(InputKey::PageUp, mods!(shift)), Action::ScrollPageUp);
            set.put(physical(InputKey::PageDown, mods!(shift)), Action::ScrollPageDown);

            // Semantic prompts
            set.put(physical(InputKey::ArrowUp, mods!(shift, ctrl)), Action::JumpToPrompt(-1));
            set.put(physical(InputKey::ArrowDown, mods!(shift, ctrl)), Action::JumpToPrompt(1));

            // Move tab
            set.put_flags(
                physical(InputKey::PageUp, mods!(shift, ctrl)),
                Action::MoveTab(-1),
                performable(true),
            );
            set.put_flags(
                physical(InputKey::PageDown, mods!(shift, ctrl)),
                Action::MoveTab(1),
                performable(true),
            );

            // Search
            set.put_flags(unicode('f', mods!(ctrl, shift)), Action::StartSearch, performable(true));
            set.put_flags(physical(InputKey::Escape, mods!()), Action::EndSearch, performable(true));

            // Inspector, matching Chromium
            set.put(unicode('i', mods!(shift, ctrl)), Action::Inspector(InspectorMode::Toggle));

            // Terminal
            set.put(unicode('a', mods!(shift, ctrl)), Action::SelectAll);

            // Selection clipboard paste
            set.put(physical(InputKey::Insert, mods!(shift)), Action::PasteFromSelection);
        }
        {
            // On macOS we default to super but everywhere else
            // is alt.
            let mods: Mods = if IS_DARWIN { mods!(super_) } else { mods!(alt) };

            // Cmd/Alt+N for goto tab N
            const DIGITS: [InputKey; 8] = [
                InputKey::Digit1,
                InputKey::Digit2,
                InputKey::Digit3,
                InputKey::Digit4,
                InputKey::Digit5,
                InputKey::Digit6,
                InputKey::Digit7,
                InputKey::Digit8,
            ];
            let start: u32 = '1' as u32;
            let end: u32 = '8' as u32;
            let mut i: u32 = start;
            while i <= end {
                // We register BOTH the physical `digit_N` key and the unicode
                // `N` key. This allows most keyboard layouts to work with
                // this shortcut. Namely, AZERTY doesn't produce unicode `N`
                // for their digit keys (they're on shifted keys on the same
                // physical keys).

                set.put_flags(
                    physical(DIGITS[(i - start) as usize], mods),
                    Action::GotoTab(((i - start) + 1) as usize),
                    // On macOS we keep this not performable so that the
                    // keyboard shortcuts in tabs work. In the future the
                    // correct fix is to fix the reverse mapping lookup
                    // to allow us to lookup performable keybinds
                    // conditionally.
                    performable(!IS_DARWIN),
                );

                // Important: this must be the LAST binding set so that the
                // libghostty trigger API returns this one for the action,
                // so that things like the macOS tab bar key equivalent label
                // work properly.
                set.put_flags(
                    Trigger {
                        key: TriggerKey::Unicode(i),
                        mods,
                    },
                    Action::GotoTab(((i - start) + 1) as usize),
                    performable(!IS_DARWIN),
                );
                i += 1;
            }
            set.put_flags(
                unicode('9', mods),
                Action::LastTab,
                // See comment above with the numeric goto_tab
                performable(!IS_DARWIN),
            );
        }

        // Toggle fullscreen
        set.put(physical(InputKey::Enter, ctrl_or_super(mods!())), Action::ToggleFullscreen);

        // Toggle zoom a split
        set.put(physical(InputKey::Enter, ctrl_or_super(mods!(shift))), Action::ToggleSplitZoom);

        // Toggle command palette, matches VSCode
        set.put(unicode('p', ctrl_or_super(mods!(shift))), Action::ToggleCommandPalette);

        // Mac-specific keyboard bindings.
        if IS_DARWIN {
            set.put(unicode('q', mods!(super_)), Action::Quit);
            set.put_flags(unicode('k', mods!(super_)), Action::ClearScreen, performable(true));
            set.put(unicode('a', mods!(super_)), Action::SelectAll);

            // Undo/redo
            set.put_flags(unicode('t', mods!(super_, shift)), Action::Undo, performable(true));
            set.put_flags(unicode('z', mods!(super_)), Action::Undo, performable(true));
            set.put_flags(unicode('z', mods!(super_, shift)), Action::Redo, performable(true));

            // Viewport scrolling
            set.put(physical(InputKey::Home, mods!(super_)), Action::ScrollToTop);
            set.put(physical(InputKey::End, mods!(super_)), Action::ScrollToBottom);
            set.put(physical(InputKey::PageUp, mods!(super_)), Action::ScrollPageUp);
            set.put(physical(InputKey::PageDown, mods!(super_)), Action::ScrollPageDown);
            set.put_flags(unicode('j', mods!(super_)), Action::ScrollToSelection, performable(true));

            // Semantic prompts
            set.put(physical(InputKey::ArrowUp, mods!(super_, shift)), Action::JumpToPrompt(-1));
            set.put(physical(InputKey::ArrowDown, mods!(super_, shift)), Action::JumpToPrompt(1));

            // Mac windowing
            set.put(unicode('n', mods!(super_)), Action::NewWindow);
            set.put(unicode('w', mods!(super_)), Action::CloseSurface);
            set.put(unicode('w', mods!(super_, alt)), Action::CloseTab(CloseTabMode::This));
            set.put(unicode('w', mods!(super_, shift)), Action::CloseWindow);
            set.put(unicode('w', mods!(super_, shift, alt)), Action::CloseAllWindows);
            set.put(unicode('t', mods!(super_)), Action::NewTab);
            set.put(unicode('[', mods!(super_, shift)), Action::PreviousTab);
            set.put(unicode(']', mods!(super_, shift)), Action::NextTab);
            set.put(unicode('d', mods!(super_)), Action::NewSplit(SplitDirection::Right));
            set.put(unicode('d', mods!(super_, shift)), Action::NewSplit(SplitDirection::Down));
            set.put(unicode('[', mods!(super_)), Action::GotoSplit(SplitFocusDirection::Previous));
            set.put(unicode(']', mods!(super_)), Action::GotoSplit(SplitFocusDirection::Next));
            set.put(
                physical(InputKey::ArrowUp, mods!(super_, alt)),
                Action::GotoSplit(SplitFocusDirection::Up),
            );
            set.put(
                physical(InputKey::ArrowDown, mods!(super_, alt)),
                Action::GotoSplit(SplitFocusDirection::Down),
            );
            set.put(
                physical(InputKey::ArrowLeft, mods!(super_, alt)),
                Action::GotoSplit(SplitFocusDirection::Left),
            );
            set.put(
                physical(InputKey::ArrowRight, mods!(super_, alt)),
                Action::GotoSplit(SplitFocusDirection::Right),
            );
            set.put(
                physical(InputKey::ArrowUp, mods!(super_, ctrl)),
                Action::ResizeSplit((SplitResizeDirection::Up, 10)),
            );
            set.put(
                physical(InputKey::ArrowDown, mods!(super_, ctrl)),
                Action::ResizeSplit((SplitResizeDirection::Down, 10)),
            );
            set.put(
                physical(InputKey::ArrowLeft, mods!(super_, ctrl)),
                Action::ResizeSplit((SplitResizeDirection::Left, 10)),
            );
            set.put(
                physical(InputKey::ArrowRight, mods!(super_, ctrl)),
                Action::ResizeSplit((SplitResizeDirection::Right, 10)),
            );
            set.put(unicode('=', mods!(super_, ctrl)), Action::EqualizeSplits);

            // Jump to prompt, matches Terminal.app
            set.put(physical(InputKey::ArrowUp, mods!(super_)), Action::JumpToPrompt(-1));
            set.put(physical(InputKey::ArrowDown, mods!(super_)), Action::JumpToPrompt(1));

            // Search
            set.put_flags(unicode('f', mods!(super_)), Action::StartSearch, performable(true));
            set.put_flags(unicode('e', mods!(super_)), Action::SearchSelection, performable(true));
            set.put_flags(unicode('f', mods!(super_, shift)), Action::EndSearch, performable(true));
            set.put_flags(physical(InputKey::Escape, mods!()), Action::EndSearch, performable(true));
            set.put_flags(
                unicode('g', mods!(super_)),
                Action::NavigateSearch(NavigateSearch::Next),
                performable(true),
            );
            set.put_flags(
                unicode('g', mods!(super_, shift)),
                Action::NavigateSearch(NavigateSearch::Previous),
                performable(true),
            );

            // Inspector, matching Chromium
            set.put(unicode('i', mods!(alt, super_)), Action::Inspector(InspectorMode::Toggle));

            // Alternate keybind, common to Mac programs
            set.put(unicode('f', mods!(super_, ctrl)), Action::ToggleFullscreen);

            // Selection clipboard paste, matches Terminal.app
            set.put(unicode('v', mods!(super_, shift)), Action::PasteFromSelection);

            // "Natural text editing" keybinds. This forces these keys to go back
            // to legacy encoding (not fixterms). It seems macOS users more than
            // others are used to these keys so we set them as defaults. If
            // people want to get back to the fixterm encoding they can set
            // the keybinds to `unbind`.
            set.put(physical(InputKey::ArrowRight, mods!(super_)), Action::Text(br"\x05".to_vec()));
            set.put(physical(InputKey::ArrowLeft, mods!(super_)), Action::Text(br"\x01".to_vec()));
            set.put(physical(InputKey::Backspace, mods!(super_)), Action::Text(br"\x15".to_vec()));
            set.put(physical(InputKey::ArrowLeft, mods!(alt)), Action::Esc(b"b".to_vec()));
            set.put(physical(InputKey::ArrowRight, mods!(alt)), Action::Esc(b"f".to_vec()));
        }
    }

    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(&self, other: &Keybinds) -> bool {
        if !Self::equal_set(&self.set, &other.set) {
            return false;
        }

        // Compare tables
        if self.tables.len() != other.tables.len() {
            return false;
        }

        for (name, set) in &self.tables {
            let Some(other_set) = other.tables.get(name) else {
                return false;
            };
            if !Self::equal_set(set, other_set) {
                return false;
            }
        }

        true
    }

    fn equal_set(this: &binding::Set, other: &binding::Set) -> bool {
        // Two keybinds are considered equal if their primary bindings
        // are the same. We don't compare reverse mappings and such.
        let self_map = &this.bindings;
        let other_map = &other.bindings;

        // If the count of mappings isn't identical they can't be equal
        if self_map.len() != other_map.len() {
            return false;
        }

        for (self_key, self_value) in self_map {
            // If the trigger isn't in the other map, they can't be equal
            let Some(other_entry) = other.get(self_key.0) else {
                return false;
            };

            match (self_value, other_entry.value_ptr) {
                // They're equal if both leader sets are equal.
                (binding::Value::Leader(a), binding::Value::Leader(b)) => {
                    if !Self::equal_set(a, b) {
                        return false;
                    }
                }

                // Actions are compared by field directly
                (binding::Value::Leaf(a), binding::Value::Leaf(b)) => {
                    if a != b {
                        return false;
                    }
                }

                (binding::Value::LeafChained(a), binding::Value::LeafChained(b)) => {
                    if a.flags != b.flags {
                        return false;
                    }
                    if a.actions.len() != b.actions.len() {
                        return false;
                    }
                    for (a1, a2) in a.actions.iter().zip(&b.actions) {
                        if a1 != a2 {
                            return false;
                        }
                    }
                }

                // If the entry types are different, they can't be equal
                _ => return false,
            }
        }

        true
    }

    /// Like formatEntry but has an option to include docs.
    pub fn format_entry_docs(
        &self,
        formatter: &mut EntryFormatter<'_>,
        docs: bool,
    ) -> Result<(), formatterpkg::Error> {
        if self.set.bindings.is_empty() && self.tables.is_empty() {
            return formatter.format_entry(&());
        }

        fn forward(
            formatter: &mut EntryFormatter<'_>,
            prefix: Vec<u8>,
            v: &binding::Value,
        ) -> Result<(), formatterpkg::Error> {
            let mut buffer = prefix;
            v.format_entries(&mut buffer, formatter)
        }

        // Format root set bindings
        for (k, v) in &self.set.bindings {
            if docs {
                formatter.writer.write_str("\n")?;
                let name = match v {
                    binding::Value::Leader(_) => "leader",
                    binding::Value::Leaf(_) => "leaf",
                    binding::Value::LeafChained(_) => "leaf_chained",
                };
                for &decl in binding::ActionKey::ALL {
                    if decl.as_str() == name {
                        if let Some(help) =
                            crate::input::helpgen_actions::help_strings::keybind_action(decl)
                        {
                            formatter.writer.write_str("# ")?;
                            formatter.writer.write_str(decl.as_str())?;
                            formatter.writer.write_str("\n")?;
                            for line in help.split('\n') {
                                formatter.writer.write_str("#   ")?;
                                formatter.writer.write_str(line)?;
                                formatter.writer.write_str("\n")?;
                            }
                        }
                        break;
                    }
                }
            }

            let mut writer: Vec<u8> = Vec::new();
            k.0.format_to(&mut writer);
            forward(formatter, writer, v)?;
        }

        // Format table bindings
        for (table_name, table_set) in &self.tables {
            for (k, v) in &table_set.bindings {
                let mut writer: Vec<u8> = Vec::new();
                writer.extend_from_slice(table_name.as_bytes());
                writer.push(b'/');
                k.0.format_to(&mut writer);
                forward(formatter, writer, v)?;
            }
        }

        Ok(())
    }
}

impl Clone for Keybinds {
    /// Deep copy of the struct. Required by Config.
    fn clone(&self) -> Self {
        let mut tables = indexmap::IndexMap::with_capacity(self.tables.len());
        for (key, set) in &self.tables {
            tables.insert(key.clone(), set.clone());
        }

        Keybinds {
            set: self.set.clone(),
            tables,
            chain_target: ChainTarget::Root,
        }
    }
}

impl PartialEq for Keybinds {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for Keybinds {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let value = input.ok_or(Error::ValueRequired)?;

        // Check for special values
        if value.is_empty() {
            log::info!(target: LOG_TARGET, "config has 'keybind =', using default keybinds");
            self.init();
            return Ok(());
        }

        if value == "clear" {
            // We don't clear the memory because its in the arena and unlikely
            // to be free-able anyways (since arenas can only clear the last
            // allocated value). This isn't a memory leak because the arena
            // will be freed when the config is freed.
            log::info!(target: LOG_TARGET, "config has 'keybind = clear', all keybinds cleared");
            self.set = binding::Set::default();
            self.tables = indexmap::IndexMap::new();
            self.chain_target = ChainTarget::Root;
            return Ok(());
        }

        // Check for table syntax: "name/" or "name/binding"
        // We look for '/' only before the first '=' to avoid matching
        // action arguments like "foo=text:/hello".
        let eq_idx = value.find('=').unwrap_or(value.len());
        'table: {
            let Some(slash_idx) = value[..eq_idx].find('/') else {
                break 'table;
            };
            let table_name = &value[..slash_idx];

            // Length zero is valid, so you can set `/=action` for the slash key
            if table_name.is_empty() {
                break 'table;
            }

            // Ignore '+', '>' because they can be part of sequences and
            // triggers. This lets things like `ctrl+/=action` work.
            if table_name.contains(['+', '>']) {
                break 'table;
            }

            let binding_str = &value[slash_idx + 1..];

            // Get or create the table
            let table = self.tables.entry(table_name.to_owned()).or_default();

            // If there's no binding after the slash, this is a table
            // definition/clear command
            if binding_str.is_empty() {
                log::debug!(target: LOG_TARGET, "config has 'keybind = {}/', table cleared", table_name);
                *table = binding::Set::default();
                self.chain_target = ChainTarget::Root;
                return Ok(());
            }

            // Chains are only allowed at the root level. Their target is
            // tracked globally by parse order in `self.chain_target`.
            if binding_str.starts_with("chain=") {
                return Err(Error::Other("InvalidFormat"));
            }

            // Parse and add the binding to the table
            table.parse_and_put(binding_str.as_bytes()).map_err(binding_error)?;
            self.chain_target = ChainTarget::Table(table_name.to_owned());
            return Ok(());
        }

        if value.starts_with("chain=") {
            match &self.chain_target {
                ChainTarget::Root => self.set.parse_and_put(value.as_bytes()).map_err(binding_error)?,
                ChainTarget::Table(table_name) => {
                    let Some(table) = self.tables.get_mut(table_name) else {
                        self.chain_target = ChainTarget::Root;
                        return Err(Error::Other("InvalidFormat"));
                    };
                    table.parse_and_put(value.as_bytes()).map_err(binding_error)?;
                }
            }
            return Ok(());
        }

        // Parse into default set
        self.set.parse_and_put(value.as_bytes()).map_err(binding_error)?;
        self.chain_target = ChainTarget::Root;
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }

    /// Zig: `pub fn init(self, alloc)` (used for `keybind =`).
    fn init_cli() -> Option<Result<Self, Error>> {
        let mut v = Keybinds::default();
        v.init();
        Some(Ok(v))
    }
}

impl FormatEntry for Keybinds {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        self.format_entry_docs(f, false)
    }
}

// ---------------------------------------------------------------------------
// RepeatableCommand
// ---------------------------------------------------------------------------

use crate::input::command::{Command as InputCommand, CommandC as InputCommandC};

#[derive(Debug, Default)]
pub struct RepeatableCommand {
    pub value: Vec<InputCommand>,
    pub value_c: Vec<InputCommandC>,

    /// Storage for the formatted action strings referenced by `value_c`
    /// (Zig allocates them in the config arena).
    value_c_alloc: Vec<std::ffi::CString>,
}

// SAFETY: the raw pointers in `value_c` only point to heap data owned by
// this struct (`value`, `value_c_alloc`) or to 'static data, and are only
// read through shared references.
unsafe impl Send for RepeatableCommand {}
// SAFETY: see above.
unsafe impl Sync for RepeatableCommand {}

/// ghostty_config_command_list_s (Zig `RepeatableCommand.C`)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RepeatableCommandC {
    pub commands: *const InputCommandC,
    pub len: usize,
}

impl RepeatableCommand {
    pub fn cval(&self) -> RepeatableCommandC {
        RepeatableCommandC {
            commands: self.value_c.as_ptr(),
            len: self.value_c.len(),
        }
    }

    pub fn init(&mut self) {
        self.value = Vec::new();
        self.value_c = Vec::new();
        self.value_c_alloc = Vec::new();
        let defaults = crate::input::command::defaults();
        self.value.reserve(defaults.len());
        self.value_c.reserve(defaults.len());
        for cmd in defaults {
            // Translation is currently a GTK-only feature. In particular,
            // translating these shared strings for the embedded runtime gives
            // the macOS app a localized command palette in an otherwise
            // unlocalized UI.
            let localized = if crate::build_config::APP_RUNTIME == crate::apprt::runtime::Runtime::Gtk {
                cmd.translated()
            } else {
                cmd.clone()
            };
            self.value_c.push(localized.cval(&mut self.value_c_alloc));
            self.value.push(localized);
        }
    }

    /// Compare if two of our value are equal. Required by Config.
    pub fn equal(&self, other: &RepeatableCommand) -> bool {
        if self.value.len() != other.value.len() {
            return false;
        }
        for (a, b) in self.value.iter().zip(&other.value) {
            if !a.equal(b) {
                return false;
            }
        }

        true
    }
}

impl Clone for RepeatableCommand {
    /// Deep copy of the struct. Required by Config.
    fn clone(&self) -> Self {
        let value = self.value.clone();

        // Cloning value_c directly would copy Command.C structs
        // whose string pointers still reference the source config's
        // memory — the clone must stay valid after the source is
        // freed.
        let mut value_c_alloc = Vec::new();
        let mut value_c = Vec::with_capacity(value.len());
        for item in &value {
            value_c.push(item.cval(&mut value_c_alloc));
        }

        RepeatableCommand {
            value,
            value_c,
            value_c_alloc,
        }
    }
}

impl PartialEq for RepeatableCommand {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

/// Zig `cli.args.parseAutoStruct(inputpkg.Command, alloc, input, null)`.
///
/// `input.Command` has the fields `action: Binding.Action`,
/// `title: [:0]const u8` and `description: [:0]const u8 = ""`. The action
/// is parsed with [`Action::parse`] (Zig parses the union field with the
/// generic `tag:value` rule, which is equivalent for the payload types of
/// actions used in commands).
fn parse_input_command(v: &str) -> Result<InputCommand, Error> {
    fn cstr(value: &str) -> Result<Cow<'static, std::ffi::CStr>, Error> {
        std::ffi::CString::new(value)
            .map(Cow::Owned)
            .map_err(|_| Error::InvalidValue)
    }

    let mut action: Option<Action> = None;
    let mut title: Option<Cow<'static, std::ffi::CStr>> = None;
    let mut description: Option<Cow<'static, std::ffi::CStr>> = None;

    // We split each value by "," allowing for quoting and escaping.
    let mut iter = crate::cli::comma_splitter::CommaSplitter::init(v);
    while let Some(entry) = iter.next()? {
        let (key, value) = args::auto_struct_entry(entry)?;
        match key {
            "action" => {
                // Get the union tag that is being set.
                let colon_idx = value.find(':').unwrap_or(value.len());
                let tag = args::trim(&value[..colon_idx], args::WHITESPACE);
                let input = if colon_idx < value.len() {
                    format!("{}:{}", tag, &value[colon_idx + 1..])
                } else {
                    tag.to_owned()
                };
                action = Some(Action::parse(input.as_bytes()).map_err(|_| Error::InvalidValue)?);
            }
            "title" => title = Some(cstr(&value)?),
            "description" => description = Some(cstr(&value)?),

            // No field matched
            _ => return Err(Error::InvalidValue),
        }
    }

    // Ensure all required fields are set.
    Ok(InputCommand {
        action: action.ok_or(Error::InvalidValue)?,
        title: title.ok_or(Error::InvalidValue)?,
        description: description.unwrap_or(Cow::Borrowed(c"")),
    })
}

impl ParseCli for RepeatableCommand {
    fn parse_cli(&mut self, input_: Option<&str>) -> Result<(), Error> {
        // Unset or empty input clears the list
        let input = input_.unwrap_or("");
        if input.is_empty() {
            log::info!(
                target: LOG_TARGET,
                "config has 'command-palette-entry =', using default entries"
            );
            self.init();
            return Ok(());
        }

        if input == "clear" {
            log::info!(
                target: LOG_TARGET,
                "config has 'command-palette-entry = clear', all command entries cleared"
            );
            self.value.clear();
            self.value_c.clear();
            return Ok(());
        }

        let cmd = parse_input_command(input)?;
        let cmd_c = cmd.cval(&mut self.value_c_alloc);
        self.value.push(cmd);
        self.value_c.push(cmd_c);
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        args::parse_cli_new_from_default(input)
    }

    /// Zig: `pub fn init(self, alloc)` (used for `command-palette-entry =`).
    fn init_cli() -> Option<Result<Self, Error>> {
        let mut v = RepeatableCommand::default();
        v.init();
        Some(Ok(v))
    }
}

/// Zig `std.zig.fmtString`: escape a string for use in a Zig
/// double-quoted string literal.
fn zig_fmt_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b' '..=b'~' => out.push(b as char),
            _ => {
                let _ = write!(out, "\\x{:02x}", b);
            }
        }
    }
    out
}

impl FormatEntry for RepeatableCommand {
    /// Used by Formatter
    fn format_entry(&self, f: &mut EntryFormatter<'_>) -> Result<(), formatterpkg::Error> {
        if self.value.is_empty() {
            return f.format_entry(&());
        }

        for item in &self.value {
            let mut writer = String::new();

            let _ = write!(writer, "title:\"{}\"", zig_fmt_string(item.title.to_bytes()));

            if !item.description.to_bytes().is_empty() {
                let _ = write!(
                    writer,
                    ",description:\"{}\"",
                    zig_fmt_string(item.description.to_bytes())
                );
            }

            let _ = write!(writer, ",action:\"{}\"", item.action);
            if writer.len() > 4096 {
                return Err(formatterpkg::Error::OutOfMemory);
            }

            f.format_entry(writer.as_str())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::binding::{ActionKey, Value as BindingValue};

    fn fmt_entry<T: FormatEntry + ?Sized>(name: &str, v: &T) -> String {
        let mut buf = String::new();
        v.format_entry(&mut formatterpkg::entry_formatter(name, &mut buf)).unwrap();
        buf
    }

    /// Zig's `TestIterator`: yields the given strings, no location.
    fn test_iter<'a>(data: &'a [&'a str]) -> args::SliceIterator<'a> {
        args::slice_iterator(data)
    }

    // -- handle bom ---------------------------------------------------------

    #[test]
    fn handle_bom_in_config_files() {
        {
            let data: &[u8] = b"\xef\xbb\xbfabnormal-command-exit-runtime = 2500\n";
            let mut cfg = Config::default();
            cfg.load_reader(data, "/home/ghostty/.config/ghostty/config.ghostty").unwrap();
            cfg.finalize().unwrap();

            assert!(cfg._diagnostics.empty());
            assert_eq!(2500, cfg.abnormal_command_exit_runtime);
        }

        {
            let data: &[u8] = b"abnormal-command-exit-runtime = 2500\n";
            let mut cfg = Config::default();
            cfg.load_reader(data, "/home/ghostty/.config/ghostty/config.ghostty").unwrap();
            cfg.finalize().unwrap();

            assert!(cfg._diagnostics.empty());
            assert_eq!(2500, cfg.abnormal_command_exit_runtime);
        }
    }

    // -- WorkingDirectory ---------------------------------------------------

    #[test]
    fn working_directory_parse_cli() {
        let mut wd = WorkingDirectory::Inherit;

        wd.parse_cli(Some("inherit")).unwrap();
        assert_eq!(WorkingDirectory::Inherit, wd);

        wd.parse_cli(Some("home")).unwrap();
        assert_eq!(WorkingDirectory::Home, wd);

        wd.parse_cli(Some("~/projects/ghostty")).unwrap();
        assert_eq!(Some("~/projects/ghostty"), wd.value());

        wd.parse_cli(Some("\"/tmp path\"")).unwrap();
        assert_eq!(Some("/tmp path"), wd.value());
    }

    #[test]
    fn working_directory_finalize() {
        let environ_map = crate::os::environ_map();

        {
            let mut wd = WorkingDirectory::Path("~/projects/ghostty".to_owned());
            wd.finalize();

            let mut buf = [0u8; MAX_PATH_BYTES];
            let expected = crate::os::expand_home(&environ_map, "~/projects/ghostty", &mut buf)
                .map(|s| s.to_owned())
                .unwrap_or_else(|_| "~/projects/ghostty".to_owned());
            assert_eq!(expected, wd.value().unwrap());
        }
    }

    // -- Color --------------------------------------------------------------

    #[test]
    fn color_parse_cli_hex() {
        assert_eq!(Color { r: 0, g: 0, b: 0 }, Color::parse_cli(Some("#000000")).unwrap());
        assert_eq!(Color { r: 10, g: 11, b: 12 }, Color::parse_cli(Some("#0A0B0C")).unwrap());
        assert_eq!(Color { r: 10, g: 11, b: 12 }, Color::parse_cli(Some("0A0B0C")).unwrap());
        assert_eq!(Color { r: 255, g: 255, b: 255 }, Color::parse_cli(Some("FFFFFF")).unwrap());
        assert_eq!(Color { r: 255, g: 255, b: 255 }, Color::parse_cli(Some("FFF")).unwrap());
        assert_eq!(Color { r: 51, g: 68, b: 85 }, Color::parse_cli(Some("#345")).unwrap());
    }

    #[test]
    fn color_parse_cli_from_name() {
        assert_eq!(Color { r: 0, g: 0, b: 0 }, Color::parse_cli(Some("black")).unwrap());
    }

    #[test]
    fn color_format_config() {
        let color = Color { r: 10, g: 11, b: 12 };
        assert_eq!("a = #0a0b0c\n", fmt_entry("a", &color));
    }

    #[test]
    fn color_parse_cli_with_whitespace() {
        assert_eq!(
            Color { r: 0xAA, g: 0xBB, b: 0xCC },
            Color::parse_cli(Some(" #AABBCC   ")).unwrap()
        );
        assert_eq!(Color { r: 0, g: 0, b: 0 }, Color::parse_cli(Some("  black ")).unwrap());
    }

    // -- TerminalColor ------------------------------------------------------

    #[test]
    fn terminal_color_parse_cli() {
        assert_eq!(
            TerminalColor::Color(Color { r: 78, g: 42, b: 132 }),
            TerminalColor::parse_cli(Some("#4e2a84")).unwrap()
        );
        assert_eq!(
            TerminalColor::Color(Color { r: 0, g: 0, b: 0 }),
            TerminalColor::parse_cli(Some("black")).unwrap()
        );
        assert_eq!(
            TerminalColor::CellForeground,
            TerminalColor::parse_cli(Some("cell-foreground")).unwrap()
        );
        assert_eq!(
            TerminalColor::CellBackground,
            TerminalColor::parse_cli(Some("cell-background")).unwrap()
        );

        assert_eq!(Err(Error::InvalidValue), TerminalColor::parse_cli(Some("a")));
    }

    #[test]
    fn terminal_color_format_config() {
        let sc = TerminalColor::CellForeground;
        assert_eq!("a = cell-foreground\n", fmt_entry("a", &sc));
    }

    // -- BoldColor ----------------------------------------------------------

    #[test]
    fn bold_color_parse_cli() {
        assert_eq!(
            BoldColor::Color(Color { r: 78, g: 42, b: 132 }),
            BoldColor::parse_cli(Some("#4e2a84")).unwrap()
        );
        assert_eq!(
            BoldColor::Color(Color { r: 0, g: 0, b: 0 }),
            BoldColor::parse_cli(Some("black")).unwrap()
        );
        assert_eq!(BoldColor::Bright, BoldColor::parse_cli(Some("bright")).unwrap());

        assert_eq!(Err(Error::InvalidValue), BoldColor::parse_cli(Some("a")));
    }

    #[test]
    fn bold_color_format_config() {
        let sc = BoldColor::Bright;
        assert_eq!("a = bright\n", fmt_entry("a", &sc));
    }

    // -- ColorList ----------------------------------------------------------

    #[test]
    fn color_list_parse_cli() {
        let mut p = ColorList::default();
        p.parse_cli(Some("black,white")).unwrap();
        assert_eq!(2, p.colors.len());

        // Test whitespace handling
        p.parse_cli(Some("black, white")).unwrap(); // space after comma
        assert_eq!(2, p.colors.len());
        p.parse_cli(Some("black , white")).unwrap(); // spaces around comma
        assert_eq!(2, p.colors.len());
        p.parse_cli(Some(" black , white ")).unwrap(); // extra spaces at ends
        assert_eq!(2, p.colors.len());

        // Error cases
        assert_eq!(Err(Error::ValueRequired), p.parse_cli(None));
        assert_eq!(Err(Error::InvalidValue), p.parse_cli(Some(" ")));
    }

    #[test]
    fn color_list_format() {
        let mut p = ColorList::default();
        p.parse_cli(Some("black,white")).unwrap();
        assert_eq!("a = #000000,#ffffff\n", fmt_entry("a", &p));
    }

    #[test]
    fn color_list_clone() {
        let mut source = ColorList::default();
        source.parse_cli(Some("#ff0000,#00ff00,#0000ff")).unwrap();

        let cloned = source.clone();

        assert!(source.equal(&cloned));
        assert_eq!(source.colors_c.len(), cloned.colors_c.len());
        for (src_c, clone_c) in source.colors_c.iter().zip(&cloned.colors_c) {
            assert_eq!(src_c.r, clone_c.r);
            assert_eq!(src_c.g, clone_c.g);
            assert_eq!(src_c.b, clone_c.b);
        }
    }

    // -- Palette ------------------------------------------------------------

    #[test]
    fn palette_parse_cli() {
        let mut p = Palette::default();
        p.parse_cli(Some("0=#AABBCC")).unwrap();
        assert!(p.value[0].r == 0xAA);
        assert!(p.value[0].g == 0xBB);
        assert!(p.value[0].b == 0xCC);
        assert!(p.mask.is_set(0));
        assert!(!p.mask.is_set(1));
    }

    #[test]
    fn palette_parse_cli_base() {
        let mut p = Palette::default();

        p.parse_cli(Some("0b1=#014589")).unwrap();
        p.parse_cli(Some("0o7=#234567")).unwrap();
        p.parse_cli(Some("0xF=#ABCDEF")).unwrap();

        assert!(p.value[0b1].r == 0x01);
        assert!(p.value[0b1].g == 0x45);
        assert!(p.value[0b1].b == 0x89);

        assert!(p.value[0o7].r == 0x23);
        assert!(p.value[0o7].g == 0x45);
        assert!(p.value[0o7].b == 0x67);

        assert!(p.value[0xF].r == 0xAB);
        assert!(p.value[0xF].g == 0xCD);
        assert!(p.value[0xF].b == 0xEF);

        assert!(p.mask.is_set(0b1));
        assert!(p.mask.is_set(0o7));
        assert!(p.mask.is_set(0xF));
        assert!(!p.mask.is_set(0));
        assert!(!p.mask.is_set(2));
    }

    #[test]
    fn palette_parse_cli_overflow() {
        let mut p = Palette::default();
        assert_eq!(Err(Error::Other("Overflow")), p.parse_cli(Some("256=#AABBCC")));
        // Mask should remain empty since parsing failed.
        assert_eq!(0usize, p.mask.count());
    }

    #[test]
    fn palette_format_config() {
        let list = Palette::default();
        assert_eq!("a = 0=#1d1f21\n", &fmt_entry("a", &list)[0..14]);
    }

    #[test]
    fn palette_parse_cli_with_whitespace() {
        let mut p = Palette::default();
        p.parse_cli(Some("0 =  #AABBCC")).unwrap();
        p.parse_cli(Some(" 1= #DDEEFF    ")).unwrap();
        p.parse_cli(Some("  2  =  #123456 ")).unwrap();

        assert!(p.value[0].r == 0xAA);
        assert!(p.value[0].g == 0xBB);
        assert!(p.value[0].b == 0xCC);

        assert!(p.value[1].r == 0xDD);
        assert!(p.value[1].g == 0xEE);
        assert!(p.value[1].b == 0xFF);

        assert!(p.value[2].r == 0x12);
        assert!(p.value[2].g == 0x34);
        assert!(p.value[2].b == 0x56);

        assert!(p.mask.is_set(0));
        assert!(p.mask.is_set(1));
        assert!(p.mask.is_set(2));
        assert!(!p.mask.is_set(3));
    }

    // -- RepeatableString ---------------------------------------------------

    #[test]
    fn repeatable_string_parse_cli() {
        let mut list = RepeatableString::default();
        list.parse_cli(Some("A")).unwrap();
        list.parse_cli(Some("B")).unwrap();
        assert_eq!(2usize, list.list.len());

        list.parse_cli(Some("")).unwrap();
        assert_eq!(0usize, list.list.len());
    }

    #[test]
    fn repeatable_string_parse_cli_overwrite() {
        let mut list = RepeatableString::default();
        list.parse_cli(Some("A")).unwrap();

        // Set our overwrite flag
        list.overwrite_next = true;

        list.parse_cli(Some("B")).unwrap();
        assert_eq!(1usize, list.list.len());
        list.parse_cli(Some("C")).unwrap();
        assert_eq!(2usize, list.list.len());
    }

    #[test]
    fn repeatable_string_format_config_empty() {
        let list = RepeatableString::default();
        assert_eq!("a = \n", fmt_entry("a", &list));
    }

    #[test]
    fn repeatable_string_format_config_single_item() {
        let mut list = RepeatableString::default();
        list.parse_cli(Some("A")).unwrap();
        assert_eq!("a = A\n", fmt_entry("a", &list));
    }

    #[test]
    fn repeatable_string_format_config_multiple_items() {
        let mut list = RepeatableString::default();
        list.parse_cli(Some("A")).unwrap();
        list.parse_cli(Some("B")).unwrap();
        assert_eq!("a = A\na = B\n", fmt_entry("a", &list));
    }

    // -- SelectionWordChars -------------------------------------------------

    #[test]
    fn selection_word_chars_parse_cli() {
        let mut chars = SelectionWordChars::default();
        chars.parse_cli(Some(" \t;,")).unwrap();

        // Should have null + 4 characters
        assert_eq!(5usize, chars.codepoints.len());
        assert_eq!(0u32, chars.codepoints[0]);
        assert_eq!(' ' as u32, chars.codepoints[1]);
        assert_eq!('\t' as u32, chars.codepoints[2]);
        assert_eq!(';' as u32, chars.codepoints[3]);
        assert_eq!(',' as u32, chars.codepoints[4]);
    }

    #[test]
    fn selection_word_chars_parse_cli_escape_sequences() {
        // \t escape should be parsed as tab
        let mut chars = SelectionWordChars::default();
        chars.parse_cli(Some(" \\t;,")).unwrap();

        assert_eq!(5usize, chars.codepoints.len());
        assert_eq!(0u32, chars.codepoints[0]);
        assert_eq!(' ' as u32, chars.codepoints[1]);
        assert_eq!('\t' as u32, chars.codepoints[2]);
        assert_eq!(';' as u32, chars.codepoints[3]);
        assert_eq!(',' as u32, chars.codepoints[4]);
    }

    #[test]
    fn selection_word_chars_parse_cli_backslash_escape() {
        // \\ should be parsed as a single backslash
        let mut chars = SelectionWordChars::default();
        chars.parse_cli(Some("\\\\;")).unwrap();

        assert_eq!(3usize, chars.codepoints.len());
        assert_eq!(0u32, chars.codepoints[0]);
        assert_eq!('\\' as u32, chars.codepoints[1]);
        assert_eq!(';' as u32, chars.codepoints[2]);
    }

    #[test]
    fn selection_word_chars_parse_cli_unicode_escape() {
        // \u{2502} should be parsed as │
        let mut chars = SelectionWordChars::default();
        chars.parse_cli(Some("\\u{2502};")).unwrap();

        assert_eq!(3usize, chars.codepoints.len());
        assert_eq!(0u32, chars.codepoints[0]);
        assert_eq!('│' as u32, chars.codepoints[1]);
        assert_eq!(';' as u32, chars.codepoints[2]);
    }

    #[test]
    fn selection_word_chars_parse_cli_truncated_utf_8() {
        let inputs: [&[u8]; 6] = [
            b"\xc2",
            b"\xe2",
            b"\xe2\x94",
            b"\xf0",
            b"\xf0\x9f",
            b"\xf0\x9f\x98",
        ];
        for input in inputs {
            let mut chars = SelectionWordChars::default();
            assert_eq!(Err(Error::InvalidValue), chars.parse_cli_bytes(Some(input)));
        }
    }

    // -- RepeatableFontVariation --------------------------------------------

    #[test]
    fn repeatable_font_variation_parse_cli() {
        let mut list = RepeatableFontVariation::default();
        list.parse_cli(Some("wght=200")).unwrap();
        list.parse_cli(Some("slnt=-15")).unwrap();

        assert_eq!(2usize, list.list.len());
        assert_eq!(
            Variation {
                id: VariationId::init(b"wght"),
                value: 200.0,
            },
            list.list[0]
        );
        assert_eq!(
            Variation {
                id: VariationId::init(b"slnt"),
                value: -15.0,
            },
            list.list[1]
        );
    }

    #[test]
    fn repeatable_font_variation_parse_cli_with_whitespace() {
        let mut list = RepeatableFontVariation::default();
        list.parse_cli(Some("wght =200")).unwrap();
        list.parse_cli(Some("slnt= -15")).unwrap();

        assert_eq!(2usize, list.list.len());
        assert_eq!(
            Variation {
                id: VariationId::init(b"wght"),
                value: 200.0,
            },
            list.list[0]
        );
        assert_eq!(
            Variation {
                id: VariationId::init(b"slnt"),
                value: -15.0,
            },
            list.list[1]
        );
    }

    #[test]
    fn repeatable_font_variation_format_config_single() {
        let mut list = RepeatableFontVariation::default();
        list.parse_cli(Some("wght = 200")).unwrap();
        assert_eq!("a = wght=200\n", fmt_entry("a", &list));
    }

    // -- Keybinds -----------------------------------------------------------

    fn kb(keybinds: &mut Keybinds, input: &str) -> Result<(), Error> {
        keybinds.parse_cli(Some(input))
    }

    #[test]
    fn keybinds_parse_cli() {
        let mut set = Keybinds::default();
        kb(&mut set, "shift+a=copy_to_clipboard").unwrap();
        kb(&mut set, "shift+a=csi:hello").unwrap();
    }

    #[test]
    fn keybinds_format_config_single() {
        let mut list = Keybinds::default();
        kb(&mut list, "shift+a=csi:hello").unwrap();
        assert_eq!("a = shift+a=csi:hello\n", fmt_entry("a", &list));
    }

    // Regression test for https://github.com/ghostty-org/ghostty/issues/2734
    #[test]
    fn keybinds_format_config_multiple_items() {
        let mut list = Keybinds::default();
        kb(&mut list, "ctrl+z>1=goto_tab:1").unwrap();
        kb(&mut list, "ctrl+z>2=goto_tab:2").unwrap();

        // Note they turn into translated keys because they match
        // their ASCII mapping.
        let want = "keybind = ctrl+z>1=goto_tab:1\nkeybind = ctrl+z>2=goto_tab:2\n";
        assert_eq!(want, fmt_entry("keybind", &list));
    }

    #[test]
    fn keybinds_format_config_multiple_items_nested() {
        let mut list = Keybinds::default();
        kb(&mut list, "ctrl+a>ctrl+b>n=new_window").unwrap();
        kb(&mut list, "ctrl+a>ctrl+b>w=close_window").unwrap();
        kb(&mut list, "ctrl+a>ctrl+c>t=new_tab").unwrap();
        kb(&mut list, "ctrl+b>ctrl+d>a=previous_tab").unwrap();

        // NB: This does not currently retain the order of the keybinds.
        let want = "a = ctrl+a>ctrl+b>n=new_window\n\
                    a = ctrl+a>ctrl+b>w=close_window\n\
                    a = ctrl+a>ctrl+c>t=new_tab\n\
                    a = ctrl+b>ctrl+d>a=previous_tab\n";
        assert_eq!(want, fmt_entry("a", &list));
    }

    #[test]
    fn keybinds_parse_cli_table_definition() {
        let mut keybinds = Keybinds::default();

        // Define a table by adding a binding to it
        kb(&mut keybinds, "foo/shift+a=copy_to_clipboard").unwrap();
        assert_eq!(1, keybinds.tables.len());
        assert!(keybinds.tables.contains_key("foo"));

        let table = keybinds.tables.get("foo").unwrap();
        assert_eq!(1, table.bindings.len());
    }

    #[test]
    fn keybinds_parse_cli_table_clear() {
        let mut keybinds = Keybinds::default();

        // Add a binding to a table
        kb(&mut keybinds, "foo/shift+a=copy_to_clipboard").unwrap();
        assert_eq!(1, keybinds.tables.get("foo").unwrap().bindings.len());

        // Clear the table with "foo/"
        kb(&mut keybinds, "foo/").unwrap();
        assert_eq!(0, keybinds.tables.get("foo").unwrap().bindings.len());
    }

    #[test]
    fn keybinds_parse_cli_table_multiple_bindings() {
        let mut keybinds = Keybinds::default();

        kb(&mut keybinds, "foo/shift+a=copy_to_clipboard").unwrap();
        kb(&mut keybinds, "foo/shift+b=paste_from_clipboard").unwrap();
        kb(&mut keybinds, "bar/ctrl+c=close_window").unwrap();

        assert_eq!(2, keybinds.tables.len());
        assert_eq!(2, keybinds.tables.get("foo").unwrap().bindings.len());
        assert_eq!(1, keybinds.tables.get("bar").unwrap().bindings.len());
    }

    #[test]
    fn keybinds_parse_cli_table_does_not_affect_root_set() {
        let mut keybinds = Keybinds::default();

        kb(&mut keybinds, "shift+a=copy_to_clipboard").unwrap();
        kb(&mut keybinds, "foo/shift+b=paste_from_clipboard").unwrap();

        // Root set should have the first binding
        assert_eq!(1, keybinds.set.bindings.len());
        // Table should have the second binding
        assert_eq!(1, keybinds.tables.get("foo").unwrap().bindings.len());
    }

    #[test]
    fn keybinds_parse_cli_table_empty_name_is_invalid() {
        let mut keybinds = Keybinds::default();
        assert_eq!(
            Err(Error::Other("InvalidFormat")),
            kb(&mut keybinds, "/shift+a=copy_to_clipboard")
        );
    }

    #[test]
    fn keybinds_parse_cli_table_with_key_sequence() {
        let mut keybinds = Keybinds::default();

        // Key sequences should work within tables
        kb(&mut keybinds, "foo/ctrl+a>ctrl+b=new_window").unwrap();

        let table = keybinds.tables.get("foo").unwrap();
        assert_eq!(1, table.bindings.len());
    }

    #[test]
    fn keybinds_parse_cli_slash_in_action_argument_is_not_a_table() {
        let mut keybinds = Keybinds::default();

        // A slash after the = should not be interpreted as a table delimiter
        kb(&mut keybinds, "ctrl+a=text:/hello").unwrap();

        // Should be in root set, not a table
        assert_eq!(1, keybinds.set.bindings.len());
        assert_eq!(0, keybinds.tables.len());
    }

    #[test]
    fn keybinds_parse_cli_slash_as_key_with_modifier_is_not_a_table() {
        let mut keybinds = Keybinds::default();

        // ctrl+/ should be parsed as a keybind with '/' as the key, not a table
        kb(&mut keybinds, "ctrl+/=text:foo").unwrap();

        // Should be in root set, not a table
        assert_eq!(1, keybinds.set.bindings.len());
        assert_eq!(0, keybinds.tables.len());
    }

    #[test]
    fn keybinds_parse_cli_shift_slash_as_key_is_not_a_table() {
        let mut keybinds = Keybinds::default();

        // shift+/ should be parsed as a keybind, not a table
        kb(&mut keybinds, "shift+/=ignore").unwrap();

        // Should be in root set, not a table
        assert_eq!(1, keybinds.set.bindings.len());
        assert_eq!(0, keybinds.tables.len());
    }

    #[test]
    fn keybinds_parse_cli_bare_slash_as_key_is_not_a_table() {
        let mut keybinds = Keybinds::default();

        // Bare / as a key should work (empty table name is rejected)
        kb(&mut keybinds, "/=text:foo").unwrap();

        // Should be in root set, not a table
        assert_eq!(1, keybinds.set.bindings.len());
        assert_eq!(0, keybinds.tables.len());
    }

    #[test]
    fn keybinds_parse_cli_slash_in_key_sequence_is_not_a_table() {
        let mut keybinds = Keybinds::default();

        // Key sequence ending with / should work
        kb(&mut keybinds, "ctrl+a>ctrl+/=new_window").unwrap();

        // Should be in root set, not a table
        assert_eq!(1, keybinds.set.bindings.len());
        assert_eq!(0, keybinds.tables.len());
    }

    #[test]
    fn keybinds_parse_cli_table_with_slash_in_binding() {
        let mut keybinds = Keybinds::default();

        // Table with a binding that uses / as the key
        kb(&mut keybinds, "mytable//=text:foo").unwrap();

        // Should be in the table
        assert_eq!(0, keybinds.set.bindings.len());
        assert_eq!(1, keybinds.tables.len());
        assert!(keybinds.tables.contains_key("mytable"));
        assert_eq!(1, keybinds.tables.get("mytable").unwrap().bindings.len());
    }

    #[test]
    fn keybinds_parse_cli_table_with_sequence_containing_slash() {
        let mut keybinds = Keybinds::default();

        // Table with a key sequence that ends with /
        kb(&mut keybinds, "mytable/a>/=new_window").unwrap();

        // Should be in the table
        assert_eq!(0, keybinds.set.bindings.len());
        assert_eq!(1, keybinds.tables.len());
        assert!(keybinds.tables.contains_key("mytable"));
    }

    #[test]
    fn keybinds_parse_cli_chain_without_prior_parsed_binding_is_invalid() {
        let mut keybinds = Keybinds::default();

        assert_eq!(Err(Error::Other("InvalidFormat")), kb(&mut keybinds, "chain=new_tab"));
    }

    #[test]
    fn keybinds_parse_cli_table_chain_syntax_is_invalid() {
        let mut keybinds = Keybinds::default();

        kb(&mut keybinds, "foo/a=text:hello").unwrap();
        assert_eq!(
            Err(Error::Other("InvalidFormat")),
            kb(&mut keybinds, "foo/chain=deactivate_key_table")
        );
    }

    #[test]
    fn keybinds_parse_cli_chain_applies_to_most_recent_table_binding() {
        let mut keybinds = Keybinds::default();

        kb(&mut keybinds, "ctrl+n=activate_key_table:foo").unwrap();
        kb(&mut keybinds, "foo/a=text:hello").unwrap();
        kb(&mut keybinds, "chain=deactivate_key_table").unwrap();

        let root_entry = keybinds
            .set
            .get(Trigger {
                mods: mods!(ctrl),
                key: TriggerKey::Unicode('n' as u32),
            })
            .unwrap()
            .value_ptr;
        let BindingValue::Leaf(leaf) = root_entry else {
            panic!("expected leaf");
        };
        assert_eq!(ActionKey::ActivateKeyTable, leaf.action.key());

        let foo_entry = keybinds
            .tables
            .get("foo")
            .unwrap()
            .get(Trigger {
                key: TriggerKey::Unicode('a' as u32),
                mods: Mods::EMPTY,
            })
            .unwrap()
            .value_ptr;
        let BindingValue::LeafChained(chained) = foo_entry else {
            panic!("expected leaf_chained");
        };
        assert_eq!(2usize, chained.actions.len());
        assert_eq!(ActionKey::Text, chained.actions[0].key());
        assert_eq!(ActionKey::DeactivateKeyTable, chained.actions[1].key());
    }

    #[test]
    fn keybinds_clone_with_tables() {
        let mut keybinds = Keybinds::default();
        kb(&mut keybinds, "shift+a=copy_to_clipboard").unwrap();
        kb(&mut keybinds, "foo/shift+b=paste_from_clipboard").unwrap();
        kb(&mut keybinds, "bar/ctrl+c=close_window").unwrap();

        let cloned = keybinds.clone();

        // Verify the clone has the same structure
        assert_eq!(keybinds.set.bindings.len(), cloned.set.bindings.len());
        assert_eq!(keybinds.tables.len(), cloned.tables.len());
        assert_eq!(
            keybinds.tables.get("foo").unwrap().bindings.len(),
            cloned.tables.get("foo").unwrap().bindings.len()
        );
        assert_eq!(
            keybinds.tables.get("bar").unwrap().bindings.len(),
            cloned.tables.get("bar").unwrap().bindings.len()
        );
    }

    #[test]
    fn keybinds_equal_with_tables() {
        let mut keybinds1 = Keybinds::default();
        kb(&mut keybinds1, "foo/shift+a=copy_to_clipboard").unwrap();

        let mut keybinds2 = Keybinds::default();
        kb(&mut keybinds2, "foo/shift+a=copy_to_clipboard").unwrap();

        assert!(keybinds1.equal(&keybinds2));
    }

    #[test]
    fn keybinds_equal_with_tables_different_table_count() {
        let mut keybinds1 = Keybinds::default();
        kb(&mut keybinds1, "foo/shift+a=copy_to_clipboard").unwrap();

        let mut keybinds2 = Keybinds::default();
        kb(&mut keybinds2, "foo/shift+a=copy_to_clipboard").unwrap();
        kb(&mut keybinds2, "bar/shift+b=paste_from_clipboard").unwrap();

        assert!(!keybinds1.equal(&keybinds2));
    }

    #[test]
    fn keybinds_equal_with_tables_different_table_names() {
        let mut keybinds1 = Keybinds::default();
        kb(&mut keybinds1, "foo/shift+a=copy_to_clipboard").unwrap();

        let mut keybinds2 = Keybinds::default();
        kb(&mut keybinds2, "bar/shift+a=copy_to_clipboard").unwrap();

        assert!(!keybinds1.equal(&keybinds2));
    }

    #[test]
    fn keybinds_equal_with_tables_different_bindings() {
        let mut keybinds1 = Keybinds::default();
        kb(&mut keybinds1, "foo/shift+a=copy_to_clipboard").unwrap();

        let mut keybinds2 = Keybinds::default();
        kb(&mut keybinds2, "foo/shift+b=paste_from_clipboard").unwrap();

        assert!(!keybinds1.equal(&keybinds2));
    }

    #[test]
    fn keybinds_format_entry_with_tables() {
        let mut keybinds = Keybinds::default();
        kb(&mut keybinds, "foo/shift+a=csi:hello").unwrap();

        assert_eq!("keybind = foo/shift+a=csi:hello\n", fmt_entry("keybind", &keybinds));
    }

    #[test]
    fn keybinds_format_entry_with_tables_and_root_set() {
        let mut keybinds = Keybinds::default();
        kb(&mut keybinds, "shift+b=csi:world").unwrap();
        kb(&mut keybinds, "foo/shift+a=csi:hello").unwrap();

        let output = fmt_entry("keybind", &keybinds);
        assert!(output.contains("keybind = shift+b=csi:world\n"));
        assert!(output.contains("keybind = foo/shift+a=csi:hello\n"));
    }

    #[test]
    fn keybinds_parse_cli_clear_clears_tables() {
        let mut keybinds = Keybinds::default();

        // Add bindings to root set and tables
        kb(&mut keybinds, "shift+a=copy_to_clipboard").unwrap();
        kb(&mut keybinds, "foo/shift+b=paste_from_clipboard").unwrap();
        kb(&mut keybinds, "bar/ctrl+c=close_window").unwrap();

        assert_eq!(1, keybinds.set.bindings.len());
        assert_eq!(2, keybinds.tables.len());

        // Clear all keybinds
        kb(&mut keybinds, "clear").unwrap();

        // Both root set and tables should be cleared
        assert_eq!(0, keybinds.set.bindings.len());
        assert_eq!(0, keybinds.tables.len());
    }

    #[test]
    fn keybinds_parse_cli_reset_clears_tables() {
        let mut keybinds = Keybinds::default();

        // Add bindings to tables
        kb(&mut keybinds, "foo/shift+a=copy_to_clipboard").unwrap();
        kb(&mut keybinds, "bar/shift+b=paste_from_clipboard").unwrap();

        assert_eq!(2, keybinds.tables.len());

        // Reset to defaults (empty value)
        kb(&mut keybinds, "").unwrap();

        // Tables should be cleared, root set has defaults
        assert_eq!(0, keybinds.tables.len());
        assert!(!keybinds.set.bindings.is_empty());
    }

    // -- RepeatableCodepointMap ---------------------------------------------

    #[test]
    fn repeatable_codepoint_map_parse_cli() {
        let mut list = RepeatableCodepointMap::default();
        list.parse_cli(Some("U+ABCD=Comic Sans")).unwrap();
        list.parse_cli(Some("U+0001 - U+0005=Verdana")).unwrap();
        list.parse_cli(Some("U+0006-U+0009, U+ABCD=Courier")).unwrap();

        assert_eq!(4usize, list.map.len());
        {
            let entry = &list.map.entry(0);
            assert_eq!([0xABCD, 0xABCD], entry.range);
            assert_eq!("Comic Sans", entry.descriptor.family.as_deref().unwrap());
        }
        {
            let entry = &list.map.entry(1);
            assert_eq!([1, 5], entry.range);
            assert_eq!("Verdana", entry.descriptor.family.as_deref().unwrap());
        }
        {
            let entry = &list.map.entry(2);
            assert_eq!([6, 9], entry.range);
            assert_eq!("Courier", entry.descriptor.family.as_deref().unwrap());
        }
        {
            let entry = &list.map.entry(3);
            assert_eq!([0xABCD, 0xABCD], entry.range);
            assert_eq!("Courier", entry.descriptor.family.as_deref().unwrap());
        }
    }

    #[test]
    fn repeatable_codepoint_map_format_config_single() {
        let mut list = RepeatableCodepointMap::default();
        list.parse_cli(Some("U+ABCD=Comic Sans")).unwrap();
        assert_eq!("a = U+ABCD=Comic Sans\n", fmt_entry("a", &list));
    }

    #[test]
    fn repeatable_codepoint_map_format_config_range() {
        let mut list = RepeatableCodepointMap::default();
        list.parse_cli(Some("U+0001 - U+0005=Verdana")).unwrap();
        assert_eq!("a = U+0001-U+0005=Verdana\n", fmt_entry("a", &list));
    }

    #[test]
    fn repeatable_codepoint_map_format_config_multiple() {
        let mut list = RepeatableCodepointMap::default();
        list.parse_cli(Some("U+0006-U+0009, U+ABCD=Courier")).unwrap();
        assert_eq!("a = U+0006-U+0009=Courier\na = U+ABCD=Courier\n", fmt_entry("a", &list));
    }

    // -- RepeatableClipboardCodepointMap ------------------------------------

    #[test]
    fn repeatable_clipboard_codepoint_map_parse_cli_codepoint_replacement() {
        let mut list = RepeatableClipboardCodepointMap::default();
        list.parse_cli(Some("U+2500=U+002D")).unwrap(); // box drawing → hyphen

        assert_eq!(1usize, list.map.list.len());
        let entry = &list.map.list[0];
        assert_eq!([0x2500, 0x2500], entry.range);
        assert_eq!(clipboard_codepoint_map::Replacement::Codepoint(0x002D), entry.replacement);
    }

    #[test]
    fn repeatable_clipboard_codepoint_map_parse_cli_string_replacement() {
        let mut list = RepeatableClipboardCodepointMap::default();
        list.parse_cli(Some("U+03A3=SUM")).unwrap(); // Greek sigma → "SUM"

        assert_eq!(1usize, list.map.list.len());
        let entry = &list.map.list[0];
        assert_eq!([0x03A3, 0x03A3], entry.range);
        assert_eq!(
            clipboard_codepoint_map::Replacement::String("SUM".to_owned()),
            entry.replacement
        );
    }

    #[test]
    fn repeatable_clipboard_codepoint_map_parse_cli_range_replacement() {
        let mut list = RepeatableClipboardCodepointMap::default();
        list.parse_cli(Some("U+2500-U+2503=|")).unwrap(); // box drawing range → pipe

        assert_eq!(1usize, list.map.list.len());
        let entry = &list.map.list[0];
        assert_eq!([0x2500, 0x2503], entry.range);
        assert_eq!(
            clipboard_codepoint_map::Replacement::String("|".to_owned()),
            entry.replacement
        );
    }

    #[test]
    fn repeatable_clipboard_codepoint_map_format_config_codepoint() {
        let mut list = RepeatableClipboardCodepointMap::default();
        list.parse_cli(Some("U+2500=U+002D")).unwrap();
        assert_eq!("a = U+2500=U+002D\n", fmt_entry("a", &list));
    }

    #[test]
    fn repeatable_clipboard_codepoint_map_format_config_string() {
        let mut list = RepeatableClipboardCodepointMap::default();
        list.parse_cli(Some("U+03A3=SUM")).unwrap();
        assert_eq!("a = U+03A3=SUM\n", fmt_entry("a", &list));
    }

    // -- FontStyle ----------------------------------------------------------

    #[test]
    fn font_style_parse_cli() {
        let mut p = FontStyle::Default;
        p.parse_cli(Some("default")).unwrap();
        assert_eq!(FontStyle::Default, p);

        p.parse_cli(Some("false")).unwrap();
        assert_eq!(FontStyle::False, p);

        p.parse_cli(Some("bold")).unwrap();
        assert_eq!(Some(&StringZ::from("bold")), p.name_value());
    }

    #[test]
    fn font_style_format_config_default() {
        let mut p = FontStyle::Default;
        p.parse_cli(Some("default")).unwrap();
        assert_eq!("a = default\n", fmt_entry("a", &p));
    }

    #[test]
    fn font_style_format_config_false() {
        let mut p = FontStyle::Default;
        p.parse_cli(Some("false")).unwrap();
        assert_eq!("a = false\n", fmt_entry("a", &p));
    }

    #[test]
    fn font_style_format_config_named() {
        let mut p = FontStyle::Default;
        p.parse_cli(Some("bold")).unwrap();
        assert_eq!("a = bold\n", fmt_entry("a", &p));
    }

    // -- RepeatableCommand --------------------------------------------------

    #[test]
    fn repeatable_command_parse_cli() {
        let mut list = RepeatableCommand::default();
        list.parse_cli(Some("title:Foo,action:ignore")).unwrap();
        list.parse_cli(Some("title:Bar,description:bobr,action:text:ale bydle")).unwrap();
        list.parse_cli(Some("title:Quux,description:boo,action:increase_font_size:2.5")).unwrap();
        list.parse_cli(Some("title:Baz,description:Raspberry Pie,action:set_font_size:3.14")).unwrap();

        assert_eq!(4usize, list.value.len());

        assert_eq!(Action::Ignore, list.value[0].action);
        assert_eq!(b"Foo", list.value[0].title.to_bytes());

        assert_eq!(Action::Text(b"ale bydle".to_vec()), list.value[1].action);
        assert_eq!(b"Bar", list.value[1].title.to_bytes());
        assert_eq!(b"bobr", list.value[1].description.to_bytes());

        assert_eq!(Action::IncreaseFontSize(2.5), list.value[2].action);
        assert_eq!(b"Quux", list.value[2].title.to_bytes());
        assert_eq!(b"boo", list.value[2].description.to_bytes());

        assert_eq!(Action::SetFontSize(3.14), list.value[3].action);
        assert_eq!(b"Baz", list.value[3].title.to_bytes());
        assert_eq!(b"Raspberry Pie", list.value[3].description.to_bytes());

        list.parse_cli(Some("clear")).unwrap();
        assert_eq!(0usize, list.value.len());

        list.parse_cli(Some("")).unwrap();
        assert_eq!(crate::input::command::defaults().len(), list.value.len());
    }

    #[test]
    fn repeatable_command_clone_rebuilds_the_c_mirror() {
        let mut list = RepeatableCommand::default();
        list.parse_cli(Some("title:Foo,description:bar,action:new_tab")).unwrap();

        let copy = list.clone();
        assert_eq!(list.value_c.len(), copy.value_c.len());
        // The clone's C strings must not alias the source's — the
        // source config can be freed while the clone lives on.
        assert!(list.value_c[0].title != copy.value_c[0].title);
        let a = unsafe { std::ffi::CStr::from_ptr(list.value_c[0].title) };
        let b = unsafe { std::ffi::CStr::from_ptr(copy.value_c[0].title) };
        assert_eq!(a, b);
    }

    #[test]
    fn repeatable_command_format_config_empty() {
        let list = RepeatableCommand::default();
        assert_eq!("a = \n", fmt_entry("a", &list));
    }

    #[test]
    fn repeatable_command_format_config_single_item() {
        let mut list = RepeatableCommand::default();
        list.parse_cli(Some("title:Bobr, action:text:Bober")).unwrap();
        assert_eq!("a = title:\"Bobr\",action:\"text:Bober\"\n", fmt_entry("a", &list));
    }

    #[test]
    fn repeatable_command_format_config_multiple_items() {
        let mut list = RepeatableCommand::default();
        list.parse_cli(Some("title:Bobr, action:text:kurwa")).unwrap();
        list.parse_cli(Some("title:Ja,   description: pierdole,  action:text:jakie bydle")).unwrap();
        assert_eq!(
            "a = title:\"Bobr\",action:\"text:kurwa\"\na = title:\"Ja\",description:\"pierdole\",action:\"text:jakie bydle\"\n",
            fmt_entry("a", &list)
        );
    }

    #[test]
    fn repeatable_command_parse_cli_commas() {
        {
            let mut list = RepeatableCommand::default();
            list.parse_cli(Some("title:\"Bo,br\",action:\"text:kur,wa\"")).unwrap();
            assert_eq!(1usize, list.value.len());

            let item = &list.value[0];
            assert_eq!(b"Bo,br", item.title.to_bytes());
            assert_eq!(b"", item.description.to_bytes());
            assert_eq!(Action::Text(b"kur,wa".to_vec()), item.action);
        }
        {
            let mut list = RepeatableCommand::default();
            list.parse_cli(Some("title:\"Bo,br\",description:\"abc,def\",action:text:kurwa")).unwrap();
            assert_eq!(1usize, list.value.len());

            let item = &list.value[0];
            assert_eq!(b"Bo,br", item.title.to_bytes());
            assert_eq!(b"abc,def", item.description.to_bytes());
            assert_eq!(Action::Text(b"kurwa".to_vec()), item.action);
        }
    }

    #[test]
    fn repeatable_command_cval() {
        let mut list = RepeatableCommand::default();
        list.parse_cli(Some("title:Foo,action:ignore")).unwrap();
        list.parse_cli(Some("title:Bar,description:bobr,action:text:ale bydle")).unwrap();

        assert_eq!(2usize, list.value.len());
        assert_eq!(2usize, list.value_c.len());

        let cv = list.cval();
        assert_eq!(2usize, cv.len);

        let s = |p: *const std::ffi::c_char| unsafe { std::ffi::CStr::from_ptr(p) }.to_str().unwrap();
        let commands = unsafe { std::slice::from_raw_parts(cv.commands, cv.len) };

        // First entry
        assert_eq!("Foo", s(commands[0].title));
        assert_eq!("ignore", s(commands[0].action_key));
        assert_eq!("ignore", s(commands[0].action));

        // Second entry
        assert_eq!("Bar", s(commands[1].title));
        assert_eq!("bobr", s(commands[1].description));
        assert_eq!("text", s(commands[1].action_key));
        assert_eq!("text:ale bydle", s(commands[1].action));
    }

    #[test]
    fn repeatable_command_cval_cleared() {
        let mut list = RepeatableCommand::default();
        list.parse_cli(Some("title:Foo,action:ignore")).unwrap();
        assert_eq!(1usize, list.cval().len);

        list.parse_cli(Some("clear")).unwrap();
        assert_eq!(0usize, list.cval().len);
    }

    // -- MouseScrollMultiplier ----------------------------------------------

    #[test]
    fn mouse_scroll_multiplier_parse() {
        let epsilon = 0.00001;
        let approx = |a: f64, b: f64| assert!((a - b).abs() <= epsilon, "{a} != {b}");

        let mut args = MouseScrollMultiplier { precision: 0.1, discrete: 3.0 };
        args.parse_cli(Some("3")).unwrap();
        approx(3.0, args.precision);
        approx(3.0, args.discrete);

        args = MouseScrollMultiplier { precision: 0.1, discrete: 3.0 };
        args.parse_cli(Some("precision:1")).unwrap();
        approx(1.0, args.precision);
        approx(3.0, args.discrete);

        args = MouseScrollMultiplier { precision: 0.1, discrete: 3.0 };
        args.parse_cli(Some("discrete:5")).unwrap();
        approx(0.1, args.precision);
        approx(5.0, args.discrete);

        args = MouseScrollMultiplier { precision: 0.1, discrete: 3.0 };
        args.parse_cli(Some("precision:3,discrete:7")).unwrap();
        approx(3.0, args.precision);
        approx(7.0, args.discrete);

        args = MouseScrollMultiplier { precision: 0.1, discrete: 3.0 };
        args.parse_cli(Some("discrete:8,precision:6")).unwrap();
        approx(6.0, args.precision);
        approx(8.0, args.discrete);

        args = MouseScrollMultiplier::DEFAULT;
        assert_eq!(Err(Error::InvalidValue), args.parse_cli(Some("foo:1")));
        assert_eq!(Err(Error::InvalidValue), args.parse_cli(Some("precision:bar")));
        assert_eq!(Err(Error::InvalidValue), args.parse_cli(Some("precision:1,discrete:3,foo:5")));
        assert_eq!(Err(Error::InvalidValue), args.parse_cli(Some("precision:1,,discrete:3")));
        assert_eq!(Err(Error::InvalidValue), args.parse_cli(Some(",precision:1,discrete:3")));
    }

    #[test]
    fn format_entry_mouse_scroll_multiplier() {
        let args = MouseScrollMultiplier { precision: 1.5, discrete: 2.5 };
        assert_eq!(
            "mouse-scroll-multiplier = precision:1.5,discrete:2.5\n",
            fmt_entry("mouse-scroll-multiplier", &args)
        );
    }

    // -- QuickTerminalSize --------------------------------------------------

    #[test]
    fn parse_quick_terminal_size() {
        let mut v = QuickTerminalSize::default();

        v.parse_cli(Some("50%")).unwrap();
        assert_eq!(Some(QuickTerminalSizeSize::Percentage(50.0)), v.primary);
        assert_eq!(None, v.secondary);

        v.parse_cli(Some("200px")).unwrap();
        assert_eq!(Some(QuickTerminalSizeSize::Pixels(200)), v.primary);
        assert_eq!(None, v.secondary);

        v.parse_cli(Some("50%,200px")).unwrap();
        assert_eq!(Some(QuickTerminalSizeSize::Percentage(50.0)), v.primary);
        assert_eq!(Some(QuickTerminalSizeSize::Pixels(200)), v.secondary);

        assert_eq!(Err(Error::ValueRequired), v.parse_cli(None));
        assert_eq!(Err(Error::ValueRequired), v.parse_cli(Some("")));
        assert_eq!(Err(Error::ValueRequired), v.parse_cli(Some("69px,")));
        assert_eq!(Err(Error::Other("TooManyArguments")), v.parse_cli(Some("69px,42%,69px")));

        assert_eq!(Err(Error::Other("MissingUnit")), v.parse_cli(Some("420")));
        assert_eq!(Err(Error::Other("MissingUnit")), v.parse_cli(Some("bobr")));
        assert_eq!(Err(Error::InvalidValue), v.parse_cli(Some("bobr%")));
        assert_eq!(Err(Error::InvalidValue), v.parse_cli(Some("-32%")));
        assert_eq!(Err(Error::InvalidValue), v.parse_cli(Some("-69px")));
    }

    #[test]
    fn calculate_quick_terminal_size() {
        type Dimensions = QuickTerminalSizeDimensions;
        let dims_landscape = Dimensions { width: 2560, height: 1600 };
        let dims_portrait = Dimensions { width: 1600, height: 2560 };

        {
            let size = QuickTerminalSize::default();
            assert_eq!(
                Dimensions { width: 2560, height: 400 },
                size.calculate(QuickTerminalPosition::Top, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 400, height: 1600 },
                size.calculate(QuickTerminalPosition::Left, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 800, height: 400 },
                size.calculate(QuickTerminalPosition::Center, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 400, height: 800 },
                size.calculate(QuickTerminalPosition::Center, dims_portrait)
            );
        }
        {
            let size = QuickTerminalSize {
                primary: Some(QuickTerminalSizeSize::Percentage(20.0)),
                secondary: None,
            };
            assert_eq!(
                Dimensions { width: 2560, height: 320 },
                size.calculate(QuickTerminalPosition::Top, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 512, height: 1600 },
                size.calculate(QuickTerminalPosition::Left, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 512, height: 400 },
                size.calculate(QuickTerminalPosition::Center, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 400, height: 512 },
                size.calculate(QuickTerminalPosition::Center, dims_portrait)
            );
        }
        {
            let size = QuickTerminalSize {
                primary: Some(QuickTerminalSizeSize::Pixels(600)),
                secondary: None,
            };
            assert_eq!(
                Dimensions { width: 2560, height: 600 },
                size.calculate(QuickTerminalPosition::Top, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 600, height: 1600 },
                size.calculate(QuickTerminalPosition::Left, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 600, height: 400 },
                size.calculate(QuickTerminalPosition::Center, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 400, height: 600 },
                size.calculate(QuickTerminalPosition::Center, dims_portrait)
            );
        }
        {
            let size = QuickTerminalSize {
                primary: Some(QuickTerminalSizeSize::Percentage(69.0)),
                secondary: Some(QuickTerminalSizeSize::Pixels(420)),
            };
            assert_eq!(
                Dimensions { width: 420, height: 1104 },
                size.calculate(QuickTerminalPosition::Top, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 1766, height: 420 },
                size.calculate(QuickTerminalPosition::Left, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 1766, height: 420 },
                size.calculate(QuickTerminalPosition::Center, dims_landscape)
            );
            assert_eq!(
                Dimensions { width: 420, height: 1766 },
                size.calculate(QuickTerminalPosition::Center, dims_portrait)
            );
        }
    }

    // -- BackgroundBlur -----------------------------------------------------

    #[test]
    fn parse_background_blur() {
        let mut v = BackgroundBlur::False;

        v.parse_cli(None).unwrap();
        assert_eq!(BackgroundBlur::True, v);

        v.parse_cli(Some("true")).unwrap();
        assert_eq!(BackgroundBlur::True, v);

        v.parse_cli(Some("false")).unwrap();
        assert_eq!(BackgroundBlur::False, v);

        v.parse_cli(Some("42")).unwrap();
        assert_eq!(BackgroundBlur::Radius(42), v);

        v.parse_cli(Some("macos-glass-regular")).unwrap();
        assert_eq!(BackgroundBlur::MacosGlassRegular, v);

        v.parse_cli(Some("macos-glass-clear")).unwrap();
        assert_eq!(BackgroundBlur::MacosGlassClear, v);

        assert_eq!(Err(Error::InvalidValue), v.parse_cli(Some("")));
        assert_eq!(Err(Error::InvalidValue), v.parse_cli(Some("aaaa")));
        assert_eq!(Err(Error::InvalidValue), v.parse_cli(Some("420")));
    }

    // -- WindowDecoration ---------------------------------------------------

    #[test]
    fn parse_window_decoration() {
        assert_eq!(WindowDecoration::Auto, WindowDecoration::parse_cli(None).unwrap());
        assert_eq!(WindowDecoration::Auto, WindowDecoration::parse_cli(Some("true")).unwrap());
        assert_eq!(WindowDecoration::None, WindowDecoration::parse_cli(Some("false")).unwrap());
        assert_eq!(WindowDecoration::Server, WindowDecoration::parse_cli(Some("server")).unwrap());
        assert_eq!(WindowDecoration::Client, WindowDecoration::parse_cli(Some("client")).unwrap());
        assert_eq!(WindowDecoration::Auto, WindowDecoration::parse_cli(Some("auto")).unwrap());
        assert_eq!(WindowDecoration::None, WindowDecoration::parse_cli(Some("none")).unwrap());
        assert_eq!(Err(Error::InvalidValue), WindowDecoration::parse_cli(Some("")));
        assert_eq!(Err(Error::InvalidValue), WindowDecoration::parse_cli(Some("aaaa")));
    }

    // -- Theme --------------------------------------------------------------

    #[test]
    fn parse_theme() {
        // Single
        {
            let v = Theme::parse_cli_new(Some("foo")).unwrap();
            assert_eq!("foo", v.light);
            assert_eq!("foo", v.dark);
        }

        // Single whitespace
        {
            let v = Theme::parse_cli_new(Some("  foo  ")).unwrap();
            assert_eq!("foo", v.light);
            assert_eq!("foo", v.dark);
        }

        // Light/dark
        {
            let v = Theme::parse_cli_new(Some(" light:foo,  dark : bar  ")).unwrap();
            assert_eq!("foo", v.light);
            assert_eq!("bar", v.dark);
        }

        // Expand tilde to home
        {
            let environ_map = crate::os::environ_map();

            let mut home_buf = [0u8; MAX_PATH_BYTES];
            let home = crate::os::expand_home(&environ_map, "~/", &mut home_buf)
                .unwrap()
                .to_owned();

            let mut v = Theme::parse_cli_new(Some("light:~/foo, dark:~/bar")).unwrap();
            v.finalize();

            assert_eq!(format!("{}foo", home), v.light);
            assert_eq!(format!("{}bar", home), v.dark);
        }

        assert_eq!(Err(Error::ValueRequired), Theme::parse_cli_new(None));
        assert_eq!(Err(Error::ValueRequired), Theme::parse_cli_new(Some("")));
        assert_eq!(Err(Error::InvalidValue), Theme::parse_cli_new(Some("light:foo")));
        assert_eq!(Err(Error::InvalidValue), Theme::parse_cli_new(Some("dark:foo")));
    }

    // -- Duration -----------------------------------------------------------

    #[test]
    fn parse_duration() {
        for unit in Duration::UNITS {
            let t = format!("0{}", unit.name);
            let d = Duration::parse_cli(Some(&t)).unwrap();
            assert_eq!(0u64, d.duration);
        }

        for unit in Duration::UNITS {
            let t = format!("1{}", unit.name);
            let d = Duration::parse_cli(Some(&t)).unwrap();
            assert_eq!(unit.factor, d.duration);
        }

        assert_eq!(0u64, Duration::parse_cli(Some("0")).unwrap().duration);
        assert_eq!(100u64, Duration::parse_cli(Some("100ns")).unwrap().duration);
        assert_eq!(1000u64, Duration::parse_cli(Some("1µs")).unwrap().duration);
        assert_eq!(1001u64, Duration::parse_cli(Some("1µs1ns")).unwrap().duration);
        assert_eq!(1001u64, Duration::parse_cli(Some("1µs 1ns")).unwrap().duration);
        assert_eq!(1001u64, Duration::parse_cli(Some(" 1µs1ns")).unwrap().duration);
        assert_eq!(1001u64, Duration::parse_cli(Some("1µs1ns ")).unwrap().duration);
        assert_eq!(30 * NS_PER_S, Duration::parse_cli(Some("30s")).unwrap().duration);
        assert_eq!(
            u64::MAX,
            Duration::parse_cli(Some("584y 49w 23h 34m 33s 709ms 551µs 615ns"))
                .unwrap()
                .duration
        );

        // Overflow
        assert_eq!(u64::MAX, Duration::parse_cli(Some("600y")).unwrap().duration);

        // Repeated units
        assert_eq!(200u64, Duration::parse_cli(Some("100ns100ns")).unwrap().duration);

        assert_eq!(Err(Error::ValueRequired), Duration::parse_cli(None));
        assert_eq!(Err(Error::ValueRequired), Duration::parse_cli(Some("")));
        assert_eq!(Err(Error::InvalidValue), Duration::parse_cli(Some("1")));
        assert_eq!(Err(Error::InvalidValue), Duration::parse_cli(Some("s")));
        assert_eq!(Err(Error::InvalidValue), Duration::parse_cli(Some("1x")));
        assert_eq!(Err(Error::InvalidValue), Duration::parse_cli(Some("1 ")));
    }

    #[test]
    fn test_format() {
        for unit in Duration::UNITS {
            let d = Duration { duration: unit.factor };
            let actual = format!("{}", d);
            let expected = if unit.name != "us" {
                format!("1{}", unit.name)
            } else {
                "1µs".to_owned()
            };
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn test_entry_formatter() {
        let p = Duration { duration: u64::MAX };
        assert_eq!("a = 584y 49w 23h 34m 33s 709ms 551µs 615ns\n", fmt_entry("a", &p));
    }

    // -- WindowPadding ------------------------------------------------------

    #[test]
    fn parse_window_padding() {
        assert_eq!(
            WindowPadding { top_left: 100, bottom_right: 100 },
            WindowPadding::parse_cli(Some("100")).unwrap()
        );
        assert_eq!(
            WindowPadding { top_left: 100, bottom_right: 200 },
            WindowPadding::parse_cli(Some("100,200")).unwrap()
        );

        // Trim whitespace
        assert_eq!(
            WindowPadding { top_left: 100, bottom_right: 200 },
            WindowPadding::parse_cli(Some(" 100 , 200 ")).unwrap()
        );

        assert_eq!(Err(Error::ValueRequired), WindowPadding::parse_cli(None));
        assert_eq!(Err(Error::InvalidValue), WindowPadding::parse_cli(Some("")));
        assert_eq!(Err(Error::InvalidValue), WindowPadding::parse_cli(Some("a")));
    }

    // -- Config -------------------------------------------------------------

    #[test]
    fn parse_hook_invalid_command() {
        let mut cfg = Config::default();

        let data = ["foo"];
        let mut it = test_iter(&data);
        assert!(cfg.parse_manually_hook("--command", &mut it).unwrap());
        assert!(cfg.command.is_none());
    }

    #[test]
    fn parse_e_command_only() {
        let mut cfg = Config::default();

        let data = ["foo"];
        let mut it = test_iter(&data);
        assert!(!cfg.parse_manually_hook("-e", &mut it).unwrap());

        let Some(Command::Direct(cmd)) = &cfg.initial_command else {
            panic!("expected direct command");
        };
        assert_eq!(cmd.len(), 1);
        assert_eq!(cmd[0], "foo");
    }

    #[test]
    fn parse_e_command_and_args() {
        let mut cfg = Config::default();

        let data = ["echo", "foo", "bar baz"];
        let mut it = test_iter(&data);
        assert!(!cfg.parse_manually_hook("-e", &mut it).unwrap());

        let Some(Command::Direct(cmd)) = &cfg.initial_command else {
            panic!("expected direct command");
        };
        assert_eq!(cmd.len(), 3);
        assert_eq!(cmd[0], "echo");
        assert_eq!(cmd[1], "foo");
        assert_eq!(cmd[2], "bar baz");
    }

    #[test]
    fn clone_default() {
        let source = Config::default();
        let dest = source.clone();

        // Should have no changes
        let mut it = source.change_iterator(&dest);
        assert_eq!(None, it.next());
    }

    #[test]
    fn clone_preserves_conditional_state() {
        let mut a = Config::default();
        a._conditional_state.theme = conditional::Theme::Dark;
        assert_eq!(conditional::Theme::Dark, a._conditional_state.theme);
        let dest = a.clone();

        // Should have no changes
        let mut it = a.change_iterator(&dest);
        assert_eq!(None, it.next());

        // Should have the same conditional state
        assert_eq!(conditional::Theme::Dark, dest._conditional_state.theme);
    }

    /// Writes the test theme files into a temp dir and returns their
    /// real paths (Zig `td.dir.realPathFile`).
    fn write_themes(td: &crate::os::TempDir, names: &[(&str, &str)]) -> Vec<String> {
        names
            .iter()
            .map(|(name, data)| {
                let path = td.path().join(name);
                std::fs::write(&path, data).unwrap();
                std::fs::canonicalize(&path).unwrap().to_string_lossy().into_owned()
            })
            .collect()
    }

    const THEME_LIGHT: &str = include_str!("testdata/theme_light");
    const THEME_DARK: &str = include_str!("testdata/theme_dark");
    const THEME_SIMPLE: &str = include_str!("testdata/theme_simple");

    #[test]
    fn clone_can_then_change_conditional_state() {
        // This tests a particular bug sequence where:
        //   1. Load light
        //   2. Convert to dark
        //   3. Clone dark
        //   4. Convert to light
        //   5. Config is still dark (bug)

        // Setup our test theme
        let td = crate::os::TempDir::init().unwrap();
        let paths = write_themes(&td, &[("theme_light", THEME_LIGHT), ("theme_dark", THEME_DARK)]);
        let (light, dark) = (&paths[0], &paths[1]);

        let mut cfg_light = Config::default();
        let arg = format!("--theme=light:{},dark:{}", light, dark);
        let data = [arg.as_str()];
        let mut it = test_iter(&data);
        cfg_light.load_iter(&mut it).unwrap();
        cfg_light.finalize().unwrap();

        let cfg_dark = cfg_light
            .change_conditional_state(conditional::State {
                theme: conditional::Theme::Dark,
                ..Default::default()
            })
            .unwrap()
            .unwrap();

        assert_eq!(Color { r: 0xEE, g: 0xEE, b: 0xEE }, cfg_dark.background);

        let cfg_clone = cfg_dark.clone();
        assert_eq!(Color { r: 0xEE, g: 0xEE, b: 0xEE }, cfg_clone.background);

        let cfg_light2 = cfg_clone
            .change_conditional_state(conditional::State {
                theme: conditional::Theme::Light,
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        assert_eq!(Color { r: 0xFF, g: 0xFF, b: 0xFF }, cfg_light2.background);
        td.deinit();
    }

    #[test]
    fn clone_preserves_conditional_set() {
        let mut cfg = Config::default();
        let data = ["--theme=light:foo,dark:bar", "--window-theme=auto"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        let clone1 = cfg.clone();

        assert!(clone1._conditional_set.contains(conditional::Key::Theme));
    }

    #[test]
    fn working_directory_expands_tilde() {
        let environ_map = crate::os::environ_map();

        let mut cfg = Config::default();
        let data = ["--working-directory=~/projects/ghostty"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        let mut buf = [0u8; MAX_PATH_BYTES];
        let expected = crate::os::expand_home(&environ_map, "~/projects/ghostty", &mut buf)
            .map(|s| s.to_owned())
            .unwrap_or_else(|_| "~/projects/ghostty".to_owned());
        assert_eq!(expected, cfg.working_directory.as_ref().unwrap().value().unwrap());
    }

    #[test]
    fn changed() {
        let source = Config::default();
        let mut dest = source.clone();
        dest.font_thicken = true;

        assert!(source.changed(&dest, Key::FontThicken));
        assert!(!source.changed(&dest, Key::FontSize));
    }

    #[test]
    fn change_conditional_state_ignores_irrelevant_changes() {
        let mut cfg = Config::default();
        let data = ["--theme=foo"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert!(cfg
            .change_conditional_state(conditional::State {
                theme: conditional::Theme::Dark,
                ..Default::default()
            })
            .unwrap()
            .is_none());
    }

    #[test]
    fn change_conditional_state_applies_relevant_changes() {
        let mut cfg = Config::default();
        let data = ["--theme=light:foo,dark:bar"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        let cfg2 = cfg
            .change_conditional_state(conditional::State {
                theme: conditional::Theme::Dark,
                ..Default::default()
            })
            .unwrap()
            .unwrap();

        assert!(cfg2._conditional_set.contains(conditional::Key::Theme));
    }

    #[test]
    fn theme_loading() {
        // Setup our test theme
        let td = crate::os::TempDir::init().unwrap();
        let paths = write_themes(&td, &[("theme", THEME_SIMPLE)]);
        let path = &paths[0];

        let mut cfg = Config::default();
        let arg = format!("--theme={}", path);
        let data = [arg.as_str()];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert_eq!(Color { r: 0x12, g: 0x3A, b: 0xBC }, cfg.background);

        // Not a conditional theme
        assert!(!cfg._conditional_set.contains(conditional::Key::Theme));
        td.deinit();
    }

    #[test]
    fn theme_loading_preserves_conditional_state() {
        // Setup our test theme
        let td = crate::os::TempDir::init().unwrap();
        let paths = write_themes(&td, &[("theme", THEME_SIMPLE)]);
        let path = &paths[0];

        let mut cfg = Config::default();
        cfg._conditional_state = conditional::State {
            theme: conditional::Theme::Dark,
            ..Default::default()
        };
        let arg = format!("--theme={}", path);
        let data = [arg.as_str()];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert!(cfg._conditional_state.theme == conditional::Theme::Dark);
        td.deinit();
    }

    #[test]
    fn theme_priority_is_lower_than_config() {
        // Setup our test theme
        let td = crate::os::TempDir::init().unwrap();
        let paths = write_themes(&td, &[("theme", THEME_SIMPLE)]);
        let path = &paths[0];

        let mut cfg = Config::default();
        let arg = format!("--theme={}", path);
        let data = ["--background=#ABCDEF", arg.as_str()];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert_eq!(Color { r: 0xAB, g: 0xCD, b: 0xEF }, cfg.background);
        td.deinit();
    }

    #[test]
    fn theme_loading_correct_light_dark() {
        // Setup our test theme
        let td = crate::os::TempDir::init().unwrap();
        let paths = write_themes(&td, &[("theme_light", THEME_LIGHT), ("theme_dark", THEME_DARK)]);
        let (light, dark) = (&paths[0], &paths[1]);
        let arg = format!("--theme=light:{},dark:{}", light, dark);

        // Light
        {
            let mut cfg = Config::default();
            let data = [arg.as_str()];
            let mut it = test_iter(&data);
            cfg.load_iter(&mut it).unwrap();
            cfg.finalize().unwrap();

            assert_eq!(Color { r: 0xFF, g: 0xFF, b: 0xFF }, cfg.background);
        }

        // Dark
        {
            let mut cfg = Config::default();
            cfg._conditional_state = conditional::State {
                theme: conditional::Theme::Dark,
                ..Default::default()
            };
            let data = [arg.as_str()];
            let mut it = test_iter(&data);
            cfg.load_iter(&mut it).unwrap();
            cfg.finalize().unwrap();

            assert_eq!(Color { r: 0xEE, g: 0xEE, b: 0xEE }, cfg.background);
        }

        // Light to Dark
        {
            let mut cfg = Config::default();
            let data = [arg.as_str()];
            let mut it = test_iter(&data);
            cfg.load_iter(&mut it).unwrap();
            cfg.finalize().unwrap();

            let new = cfg
                .change_conditional_state(conditional::State {
                    theme: conditional::Theme::Dark,
                    ..Default::default()
                })
                .unwrap()
                .unwrap();
            assert_eq!(Color { r: 0xEE, g: 0xEE, b: 0xEE }, new.background);
        }
        td.deinit();
    }

    #[test]
    fn theme_specifying_light_dark_changes_window_theme_from_auto() {
        let mut cfg = Config::default();
        let data = ["--theme=light:foo,dark:bar", "--window-theme=auto"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert!(cfg.window_theme == WindowTheme::System);
    }

    #[test]
    fn theme_specifying_light_dark_sets_theme_usage_in_conditional_state() {
        let mut cfg = Config::default();
        let data = ["--theme=light:foo,dark:bar", "--window-theme=auto"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert!(cfg.window_theme == WindowTheme::System);
        assert!(cfg._conditional_set.contains(conditional::Key::Theme));
    }

    #[test]
    fn scrollback_limits() {
        let mut cfg = Config::default();
        assert_eq!(50_000_000usize, cfg.scrollback_limit_bytes.value);
        assert_eq!(usize::MAX, cfg.scrollback_limit_lines.value);

        let data = ["--scrollback-limit-bytes=1234", "--scrollback-limit-lines=567"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();

        assert_eq!(1234usize, cfg.scrollback_limit_bytes.value);
        assert_eq!(567usize, cfg.scrollback_limit_lines.value);

        let data = [
            "--scrollback-limit-bytes=unlimited",
            "--scrollback-limit-lines=unlimited",
        ];
        let mut unlimited_it = test_iter(&data);
        cfg.load_iter(&mut unlimited_it).unwrap();

        assert_eq!(usize::MAX, cfg.scrollback_limit_bytes.value);
        assert_eq!(usize::MAX, cfg.scrollback_limit_lines.value);

        let data = ["--scrollback-limit-bytes=", "--scrollback-limit-lines="];
        let mut reset_it = test_iter(&data);
        cfg.load_iter(&mut reset_it).unwrap();

        assert_eq!(50_000_000usize, cfg.scrollback_limit_bytes.value);
        assert_eq!(usize::MAX, cfg.scrollback_limit_lines.value);
    }

    #[test]
    fn clipboard_write_limit() {
        let mut cfg = Config::default();
        assert_eq!(64 * 1024 * 1024usize, cfg.clipboard_write_limit_bytes.value);

        let data = ["--clipboard-write-limit-bytes=1234"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();

        assert_eq!(1234usize, cfg.clipboard_write_limit_bytes.value);

        let data = ["--clipboard-write-limit-bytes=unlimited"];
        let mut unlimited_it = test_iter(&data);
        cfg.load_iter(&mut unlimited_it).unwrap();

        assert_eq!(usize::MAX, cfg.clipboard_write_limit_bytes.value);
    }

    #[test]
    fn compatibility_scrollback_limit_renamed_to_bytes() {
        let mut cfg = Config::default();
        let data = ["--scrollback-limit=1234"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();

        assert_eq!(1234usize, cfg.scrollback_limit_bytes.value);

        let data = ["--scrollback-limit=unlimited"];
        let mut unlimited_it = test_iter(&data);
        cfg.load_iter(&mut unlimited_it).unwrap();

        assert_eq!(usize::MAX, cfg.scrollback_limit_bytes.value);
    }

    #[test]
    fn compatibility_gtk_single_instance_desktop() {
        let mut cfg = Config::default();
        let data = ["--gtk-single-instance=desktop"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();

        // We need to test this BEFORE finalize, because finalize will
        // convert our detect to a real value.
        assert_eq!(GtkSingleInstance::Detect, cfg.gtk_single_instance);
    }

    #[test]
    fn compatibility_removed_cursor_invert_fg_bg() {
        let mut cfg = Config::default();
        let data = ["--cursor-invert-fg-bg"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert_eq!(Some(TerminalColor::CellForeground), cfg.cursor_color);
        assert_eq!(Some(TerminalColor::CellBackground), cfg.cursor_text);
    }

    #[test]
    fn compatibility_removed_selection_invert_fg_bg() {
        let mut cfg = Config::default();
        let data = ["--selection-invert-fg-bg"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert_eq!(Some(TerminalColor::CellBackground), cfg.selection_foreground);
        assert_eq!(Some(TerminalColor::CellForeground), cfg.selection_background);
    }

    #[test]
    fn compatibility_removed_bold_is_bright() {
        let mut cfg = Config::default();
        let data = ["--bold-is-bright"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        assert_eq!(Some(BoldColor::Bright), cfg.bold_color);
    }

    #[test]
    fn compatibility_window_new_window() {
        let mut cfg = Config::default();
        let data = ["--macos-dock-drop-behavior=window"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();
        assert_eq!(MacOSDockDropBehavior::NewWindow, cfg.macos_dock_drop_behavior);
    }

    // -- Port-specific checks -----------------------------------------------

    #[test]
    fn help_strings_and_field_order() {
        // The first field is `language`, whose help text starts as in Zig.
        assert_eq!("language", Config::FIELDS[0].name);
        assert!(Config::FIELDS[0]
            .help()
            .starts_with("Set Ghostty's graphical user interface language to a language other than the\nsystem default language."));
        assert_eq!("auto-update-channel", Config::FIELDS[Config::FIELDS.len() - 1].name);
    }

    #[test]
    fn replay_e_from_cli_through_config_file() {
        // `-e` is kept at the end of the replay steps across a config
        // reload with a changed conditional state.
        let mut cfg = Config::default();
        let data = ["--theme=light:foo,dark:bar", "-e", "echo", "hi"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();
        cfg.finalize().unwrap();

        let cfg2 = cfg
            .change_conditional_state(conditional::State {
                theme: conditional::Theme::Dark,
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let Some(Command::Direct(cmd)) = &cfg2.initial_command else {
            panic!("expected direct command");
        };
        assert_eq!(2, cmd.len());
        assert_eq!(cmd[0], "echo");
        assert_eq!(cmd[1], "hi");
    }

    #[test]
    fn load_recursive_files_with_cycle() {
        let td = crate::os::TempDir::init().unwrap();
        let dir = std::fs::canonicalize(td.path()).unwrap();
        let a = dir.join("a.ghostty");
        let b = dir.join("b.ghostty");
        std::fs::write(&a, "font-size = 20\nconfig-file = b.ghostty\n").unwrap();
        std::fs::write(&b, "background = #010203\nconfig-file = a.ghostty\nconfig-file = ?missing\n")
            .unwrap();

        let mut cfg = Config::default();
        cfg.load_file(a.to_str().unwrap()).unwrap();
        cfg.load_recursive_files().unwrap();
        cfg.finalize().unwrap();

        assert_eq!(20.0, cfg.font_size);
        assert_eq!(Color { r: 1, g: 2, b: 3 }, cfg.background);

        // a.ghostty loads b.ghostty, which loads a.ghostty again (it was
        // loaded directly, not through config-file), which then lists
        // b.ghostty a second time: a cycle. The optional missing file is
        // silently skipped.
        let messages: Vec<String> = cfg
            ._diagnostics
            .items()
            .iter()
            .map(|d| d.message.as_str().to_owned())
            .collect();
        assert_eq!(1, messages.len(), "{messages:?}");
        assert!(messages[0].ends_with("b.ghostty: cycle detected"), "{messages:?}");
        td.deinit();
    }

    #[test]
    fn load_optional_file_not_found() {
        let td = crate::os::TempDir::init().unwrap();
        let dir = std::fs::canonicalize(td.path()).unwrap();
        let missing = dir.join("missing.ghostty");
        let present = dir.join("present.ghostty");
        std::fs::write(&present, "font-thicken = true\n").unwrap();

        let mut cfg = Config::default();
        assert_eq!(OptionalFileAction::NotFound, cfg.load_optional_file(missing.to_str().unwrap()));
        assert_eq!(OptionalFileAction::Loaded, cfg.load_optional_file(present.to_str().unwrap()));
        assert!(cfg.font_thicken);
        td.deinit();
    }

    #[test]
    fn load_cli_args_config_default_files_discards_files() {
        let td = crate::os::TempDir::init().unwrap();
        let dir = std::fs::canonicalize(td.path()).unwrap();
        let file = dir.join("config.ghostty");
        std::fs::write(&file, "font-size = 20\n").unwrap();

        let mut cfg = Config::default();
        cfg.load_file(file.to_str().unwrap()).unwrap();
        assert_eq!(20.0, cfg.font_size);

        let argv: Vec<std::ffi::CString> = ["ghostty", "--config-default-files=false", "--font-thicken"]
            .iter()
            .map(|s| std::ffi::CString::new(*s).unwrap())
            .collect();
        cfg.load_cli_args_from(&argv).unwrap();

        // The file's values were discarded, the CLI's were kept.
        assert!(!cfg.config_default_files);
        assert!(cfg.font_thicken);
        assert_eq!(<Config as Default>::default().font_size, cfg.font_size);
        td.deinit();
    }

    #[test]
    fn load_cli_args_font_family_overwrites() {
        let mut cfg = Config::default();
        let data = ["--font-family=A"];
        let mut it = test_iter(&data);
        cfg.load_iter(&mut it).unwrap();

        let argv: Vec<std::ffi::CString> = ["ghostty", "--font-family=B", "--font-family=C", "-e", "echo"]
            .iter()
            .map(|s| std::ffi::CString::new(*s).unwrap())
            .collect();
        cfg.load_cli_args_from(&argv).unwrap();

        let families: Vec<&str> = cfg.font_family.list.iter().map(|s| s.as_str()).collect();
        assert_eq!(vec!["B", "C"], families);
        assert!(!cfg.font_family.overwrite_next);
        assert!(matches!(&cfg.initial_command, Some(Command::Direct(cmd)) if cmd.len() == 1));
    }
}
