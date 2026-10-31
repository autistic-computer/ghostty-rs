//! Port of `src/config/key.zig`.
//!
//! STATUS: ported
//!
//! Zig builds `Key` at comptime from the fields of `Config`. The Rust enum
//! is generated from the same field list (in the same order, so a key's
//! discriminant is its index into
//! [`Fields::FIELDS`](crate::cli::args::Fields::FIELDS)), and `Value(key)`
//! is the [`Value!`](crate::config::key::Value) macro, which names the
//! field type of a key given its Zig name.

use crate::cli::args::{cli_enum, CliEnum};

cli_enum! {
    /// Key is an enum of all the available configuration keys. This is used
    /// when paired with diff to determine what fields have changed in a config,
    /// amongst other things.
    pub enum Key {
        Language => "language",
        FontFamily => "font-family",
        FontFamilyBold => "font-family-bold",
        FontFamilyItalic => "font-family-italic",
        FontFamilyBoldItalic => "font-family-bold-italic",
        FontStyle => "font-style",
        FontStyleBold => "font-style-bold",
        FontStyleItalic => "font-style-italic",
        FontStyleBoldItalic => "font-style-bold-italic",
        FontSyntheticStyle => "font-synthetic-style",
        FontFeature => "font-feature",
        FontSize => "font-size",
        FontVariation => "font-variation",
        FontVariationBold => "font-variation-bold",
        FontVariationItalic => "font-variation-italic",
        FontVariationBoldItalic => "font-variation-bold-italic",
        FontCodepointMap => "font-codepoint-map",
        ClipboardCodepointMap => "clipboard-codepoint-map",
        FontThicken => "font-thicken",
        FontThickenStrength => "font-thicken-strength",
        FontShapingBreak => "font-shaping-break",
        AlphaBlending => "alpha-blending",
        AdjustCellWidth => "adjust-cell-width",
        AdjustCellHeight => "adjust-cell-height",
        AdjustFontBaseline => "adjust-font-baseline",
        AdjustUnderlinePosition => "adjust-underline-position",
        AdjustUnderlineThickness => "adjust-underline-thickness",
        AdjustStrikethroughPosition => "adjust-strikethrough-position",
        AdjustStrikethroughThickness => "adjust-strikethrough-thickness",
        AdjustOverlinePosition => "adjust-overline-position",
        AdjustOverlineThickness => "adjust-overline-thickness",
        AdjustCursorThickness => "adjust-cursor-thickness",
        AdjustCursorHeight => "adjust-cursor-height",
        AdjustBoxThickness => "adjust-box-thickness",
        AdjustIconHeight => "adjust-icon-height",
        GraphemeWidthMethod => "grapheme-width-method",
        FreetypeLoadFlags => "freetype-load-flags",
        Theme => "theme",
        Background => "background",
        Foreground => "foreground",
        BackgroundImage => "background-image",
        BackgroundImageOpacity => "background-image-opacity",
        BackgroundImagePosition => "background-image-position",
        BackgroundImageFit => "background-image-fit",
        BackgroundImageRepeat => "background-image-repeat",
        SelectionForeground => "selection-foreground",
        SelectionBackground => "selection-background",
        SelectionClearOnTyping => "selection-clear-on-typing",
        SelectionClearOnCopy => "selection-clear-on-copy",
        SelectionWordChars => "selection-word-chars",
        MinimumContrast => "minimum-contrast",
        Palette => "palette",
        PaletteGenerate => "palette-generate",
        PaletteHarmonious => "palette-harmonious",
        CursorColor => "cursor-color",
        CursorOpacity => "cursor-opacity",
        CursorStyle => "cursor-style",
        CursorStyleBlink => "cursor-style-blink",
        CursorText => "cursor-text",
        CursorClickToMove => "cursor-click-to-move",
        MouseHideWhileTyping => "mouse-hide-while-typing",
        ScrollToBottom => "scroll-to-bottom",
        MouseShiftCapture => "mouse-shift-capture",
        MouseReporting => "mouse-reporting",
        MouseScrollMultiplier => "mouse-scroll-multiplier",
        BackgroundOpacity => "background-opacity",
        BackgroundOpacityCells => "background-opacity-cells",
        BackgroundBlur => "background-blur",
        UnfocusedSplitOpacity => "unfocused-split-opacity",
        UnfocusedSplitFill => "unfocused-split-fill",
        SplitDividerColor => "split-divider-color",
        SplitPreserveZoom => "split-preserve-zoom",
        SearchForeground => "search-foreground",
        SearchBackground => "search-background",
        SearchSelectedForeground => "search-selected-foreground",
        SearchSelectedBackground => "search-selected-background",
        Command => "command",
        InitialCommand => "initial-command",
        NotifyOnCommandFinish => "notify-on-command-finish",
        NotifyOnCommandFinishAction => "notify-on-command-finish-action",
        NotifyOnCommandFinishAfter => "notify-on-command-finish-after",
        Env => "env",
        Input => "input",
        WaitAfterCommand => "wait-after-command",
        AbnormalCommandExitRuntime => "abnormal-command-exit-runtime",
        ScrollbackLimitBytes => "scrollback-limit-bytes",
        ScrollbackLimitLines => "scrollback-limit-lines",
        ScrollbackCompression => "scrollback-compression",
        Scrollbar => "scrollbar",
        Link => "link",
        LinkUrl => "link-url",
        LinkOsc8 => "link-osc8",
        LinkPreviews => "link-previews",
        Maximize => "maximize",
        Fullscreen => "fullscreen",
        Title => "title",
        Class => "class",
        X11InstanceName => "x11-instance-name",
        WorkingDirectory => "working-directory",
        Keybind => "keybind",
        KeyRemap => "key-remap",
        WindowPaddingX => "window-padding-x",
        WindowPaddingY => "window-padding-y",
        WindowPaddingBalance => "window-padding-balance",
        WindowPaddingColor => "window-padding-color",
        WindowVsync => "window-vsync",
        WindowInheritWorkingDirectory => "window-inherit-working-directory",
        TabInheritWorkingDirectory => "tab-inherit-working-directory",
        SplitInheritWorkingDirectory => "split-inherit-working-directory",
        WindowInheritFontSize => "window-inherit-font-size",
        WindowDecoration => "window-decoration",
        WindowTitleFontFamily => "window-title-font-family",
        WindowSubtitle => "window-subtitle",
        WindowTheme => "window-theme",
        WindowColorspace => "window-colorspace",
        WindowHeight => "window-height",
        WindowWidth => "window-width",
        WindowPositionX => "window-position-x",
        WindowPositionY => "window-position-y",
        WindowSaveState => "window-save-state",
        WindowStepResize => "window-step-resize",
        WindowNewTabPosition => "window-new-tab-position",
        WindowShowTabBar => "window-show-tab-bar",
        WindowTitlebarBackground => "window-titlebar-background",
        WindowTitlebarForeground => "window-titlebar-foreground",
        DragHandle => "drag-handle",
        ResizeOverlay => "resize-overlay",
        ResizeOverlayPosition => "resize-overlay-position",
        ResizeOverlayDuration => "resize-overlay-duration",
        FocusFollowsMouse => "focus-follows-mouse",
        ClipboardRead => "clipboard-read",
        ClipboardWrite => "clipboard-write",
        ClipboardWriteLimitBytes => "clipboard-write-limit-bytes",
        ClipboardTrimTrailingSpaces => "clipboard-trim-trailing-spaces",
        ClipboardPasteProtection => "clipboard-paste-protection",
        ClipboardPasteBracketedSafe => "clipboard-paste-bracketed-safe",
        TitleReport => "title-report",
        ImageStorageLimit => "image-storage-limit",
        CopyOnSelect => "copy-on-select",
        RightClickAction => "right-click-action",
        MiddleClickAction => "middle-click-action",
        ClickRepeatInterval => "click-repeat-interval",
        ConfigFile => "config-file",
        ConfigDefaultFiles => "config-default-files",
        ConfirmCloseSurface => "confirm-close-surface",
        QuitAfterLastWindowClosed => "quit-after-last-window-closed",
        QuitAfterLastWindowClosedDelay => "quit-after-last-window-closed-delay",
        InitialWindow => "initial-window",
        UndoTimeout => "undo-timeout",
        QuickTerminalPosition => "quick-terminal-position",
        QuickTerminalSize => "quick-terminal-size",
        GtkQuickTerminalLayer => "gtk-quick-terminal-layer",
        GtkQuickTerminalNamespace => "gtk-quick-terminal-namespace",
        QuickTerminalScreen => "quick-terminal-screen",
        QuickTerminalAnimationDuration => "quick-terminal-animation-duration",
        QuickTerminalAutohide => "quick-terminal-autohide",
        QuickTerminalSpaceBehavior => "quick-terminal-space-behavior",
        QuickTerminalKeyboardInteractivity => "quick-terminal-keyboard-interactivity",
        ShellIntegration => "shell-integration",
        ShellIntegrationFeatures => "shell-integration-features",
        CommandPaletteEntry => "command-palette-entry",
        OscColorReportFormat => "osc-color-report-format",
        VtKamAllowed => "vt-kam-allowed",
        VtWindowResizeAllowed => "vt-window-resize-allowed",
        VtXtChecksumReport => "vt-xt-checksum-report",
        VtXtChecksumExtension => "vt-xt-checksum-extension",
        CustomShader => "custom-shader",
        CustomShaderAnimation => "custom-shader-animation",
        BellFeatures => "bell-features",
        BellAudioPath => "bell-audio-path",
        BellAudioVolume => "bell-audio-volume",
        AppNotifications => "app-notifications",
        MacosNonNativeFullscreen => "macos-non-native-fullscreen",
        MacosWindowButtons => "macos-window-buttons",
        MacosTitlebarStyle => "macos-titlebar-style",
        MacosTitlebarProxyIcon => "macos-titlebar-proxy-icon",
        MacosDockDropBehavior => "macos-dock-drop-behavior",
        MacosOptionAsAlt => "macos-option-as-alt",
        MacosWindowShadow => "macos-window-shadow",
        MacosHidden => "macos-hidden",
        MacosAutoSecureInput => "macos-auto-secure-input",
        MacosSecureInputIndication => "macos-secure-input-indication",
        MacosApplescript => "macos-applescript",
        MacosIcon => "macos-icon",
        MacosCustomIcon => "macos-custom-icon",
        MacosIconFrame => "macos-icon-frame",
        MacosIconGhostColor => "macos-icon-ghost-color",
        MacosIconScreenColor => "macos-icon-screen-color",
        MacosShortcuts => "macos-shortcuts",
        LinuxCgroup => "linux-cgroup",
        LinuxCgroupMemoryLimit => "linux-cgroup-memory-limit",
        LinuxCgroupProcessesLimit => "linux-cgroup-processes-limit",
        LinuxCgroupHardFail => "linux-cgroup-hard-fail",
        GtkOpenglDebug => "gtk-opengl-debug",
        GtkSingleInstance => "gtk-single-instance",
        GtkTitlebar => "gtk-titlebar",
        GtkTabsLocation => "gtk-tabs-location",
        GtkTabsInverted => "gtk-tabs-inverted",
        GtkTitlebarHideWhenMaximized => "gtk-titlebar-hide-when-maximized",
        GtkToolbarStyle => "gtk-toolbar-style",
        GtkTitlebarStyle => "gtk-titlebar-style",
        GtkWideTabs => "gtk-wide-tabs",
        GtkHorizontalTabScroll => "gtk-horizontal-tab-scroll",
        GtkCustomCss => "gtk-custom-css",
        DesktopNotifications => "desktop-notifications",
        ProgressStyle => "progress-style",
        BoldColor => "bold-color",
        FaintOpacity => "faint-opacity",
        Term => "term",
        EnquiryResponse => "enquiry-response",
        AsyncBackend => "async-backend",
        AutoUpdate => "auto-update",
        AutoUpdateChannel => "auto-update-channel",
    }
}

impl Key {
    /// The index of this key's field in `Config::FIELDS` (Zig: the enum's
    /// integer value).
    pub fn index(self) -> usize {
        self as usize
    }

    /// The key for the given field index. Panics if out of range.
    pub fn from_index(index: usize) -> Key {
        Key::from_tag(<Key as CliEnum>::FIELD_NAMES[index]).expect("valid key index")
    }
}

/// Returns the value type for a key: `Value!("font-family")` is
/// `RepeatableString` (Zig `key.Value(.@"font-family")`).
#[macro_export]
#[doc(hidden)]
macro_rules! __config_key_value {
    ("language") => { ::core::option::Option<$crate::config::config::StringZ> };
    ("font-family") => { $crate::config::config::RepeatableString };
    ("font-family-bold") => { $crate::config::config::RepeatableString };
    ("font-family-italic") => { $crate::config::config::RepeatableString };
    ("font-family-bold-italic") => { $crate::config::config::RepeatableString };
    ("font-style") => { $crate::config::config::FontStyle };
    ("font-style-bold") => { $crate::config::config::FontStyle };
    ("font-style-italic") => { $crate::config::config::FontStyle };
    ("font-style-bold-italic") => { $crate::config::config::FontStyle };
    ("font-synthetic-style") => { $crate::config::config::FontSyntheticStyle };
    ("font-feature") => { $crate::config::config::RepeatableString };
    ("font-size") => { f32 };
    ("font-variation") => { $crate::config::config::RepeatableFontVariation };
    ("font-variation-bold") => { $crate::config::config::RepeatableFontVariation };
    ("font-variation-italic") => { $crate::config::config::RepeatableFontVariation };
    ("font-variation-bold-italic") => { $crate::config::config::RepeatableFontVariation };
    ("font-codepoint-map") => { $crate::config::config::RepeatableCodepointMap };
    ("clipboard-codepoint-map") => { $crate::config::config::RepeatableClipboardCodepointMap };
    ("font-thicken") => { bool };
    ("font-thicken-strength") => { u8 };
    ("font-shaping-break") => { $crate::config::config::FontShapingBreak };
    ("alpha-blending") => { $crate::config::config::AlphaBlending };
    ("adjust-cell-width") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-cell-height") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-font-baseline") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-underline-position") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-underline-thickness") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-strikethrough-position") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-strikethrough-thickness") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-overline-position") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-overline-thickness") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-cursor-thickness") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-cursor-height") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-box-thickness") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("adjust-icon-height") => { ::core::option::Option<$crate::config::config::MetricModifier> };
    ("grapheme-width-method") => { $crate::config::config::GraphemeWidthMethod };
    ("freetype-load-flags") => { $crate::config::config::FreetypeLoadFlags };
    ("theme") => { ::core::option::Option<$crate::config::config::Theme> };
    ("background") => { $crate::config::config::Color };
    ("foreground") => { $crate::config::config::Color };
    ("background-image") => { ::core::option::Option<$crate::config::config::Path> };
    ("background-image-opacity") => { f32 };
    ("background-image-position") => { $crate::config::config::BackgroundImagePosition };
    ("background-image-fit") => { $crate::config::config::BackgroundImageFit };
    ("background-image-repeat") => { bool };
    ("selection-foreground") => { ::core::option::Option<$crate::config::config::TerminalColor> };
    ("selection-background") => { ::core::option::Option<$crate::config::config::TerminalColor> };
    ("selection-clear-on-typing") => { bool };
    ("selection-clear-on-copy") => { bool };
    ("selection-word-chars") => { $crate::config::config::SelectionWordChars };
    ("minimum-contrast") => { f64 };
    ("palette") => { $crate::config::config::Palette };
    ("palette-generate") => { bool };
    ("palette-harmonious") => { bool };
    ("cursor-color") => { ::core::option::Option<$crate::config::config::TerminalColor> };
    ("cursor-opacity") => { f64 };
    ("cursor-style") => { $crate::config::config::CursorStyle };
    ("cursor-style-blink") => { ::core::option::Option<bool> };
    ("cursor-text") => { ::core::option::Option<$crate::config::config::TerminalColor> };
    ("cursor-click-to-move") => { bool };
    ("mouse-hide-while-typing") => { bool };
    ("scroll-to-bottom") => { $crate::config::config::ScrollToBottom };
    ("mouse-shift-capture") => { $crate::config::config::MouseShiftCapture };
    ("mouse-reporting") => { bool };
    ("mouse-scroll-multiplier") => { $crate::config::config::MouseScrollMultiplier };
    ("background-opacity") => { f64 };
    ("background-opacity-cells") => { bool };
    ("background-blur") => { $crate::config::config::BackgroundBlur };
    ("unfocused-split-opacity") => { f64 };
    ("unfocused-split-fill") => { ::core::option::Option<$crate::config::config::Color> };
    ("split-divider-color") => { ::core::option::Option<$crate::config::config::Color> };
    ("split-preserve-zoom") => { $crate::config::config::SplitPreserveZoom };
    ("search-foreground") => { $crate::config::config::TerminalColor };
    ("search-background") => { $crate::config::config::TerminalColor };
    ("search-selected-foreground") => { $crate::config::config::TerminalColor };
    ("search-selected-background") => { $crate::config::config::TerminalColor };
    ("command") => { ::core::option::Option<$crate::config::config::Command> };
    ("initial-command") => { ::core::option::Option<$crate::config::config::Command> };
    ("notify-on-command-finish") => { $crate::config::config::NotifyOnCommandFinish };
    ("notify-on-command-finish-action") => { $crate::config::config::NotifyOnCommandFinishAction };
    ("notify-on-command-finish-after") => { $crate::config::config::Duration };
    ("env") => { $crate::config::config::RepeatableStringMap };
    ("input") => { $crate::config::config::RepeatableReadableIO };
    ("wait-after-command") => { bool };
    ("abnormal-command-exit-runtime") => { u32 };
    ("scrollback-limit-bytes") => { $crate::config::config::Limit<usize, 50_000_000> };
    ("scrollback-limit-lines") => { $crate::config::config::Limit<usize, { usize::MAX as i128 }> };
    ("scrollback-compression") => { bool };
    ("scrollbar") => { $crate::config::config::Scrollbar };
    ("link") => { $crate::config::config::RepeatableLink };
    ("link-url") => { bool };
    ("link-osc8") => { bool };
    ("link-previews") => { $crate::config::config::LinkPreviews };
    ("maximize") => { bool };
    ("fullscreen") => { $crate::config::config::Fullscreen };
    ("title") => { ::core::option::Option<$crate::config::config::StringZ> };
    ("class") => { ::core::option::Option<$crate::config::config::StringZ> };
    ("x11-instance-name") => { ::core::option::Option<$crate::config::config::StringZ> };
    ("working-directory") => { ::core::option::Option<$crate::config::config::WorkingDirectory> };
    ("keybind") => { $crate::config::config::Keybinds };
    ("key-remap") => { $crate::config::config::KeyRemapSet };
    ("window-padding-x") => { $crate::config::config::WindowPadding };
    ("window-padding-y") => { $crate::config::config::WindowPadding };
    ("window-padding-balance") => { $crate::config::config::WindowPaddingBalance };
    ("window-padding-color") => { $crate::config::config::WindowPaddingColor };
    ("window-vsync") => { bool };
    ("window-inherit-working-directory") => { bool };
    ("tab-inherit-working-directory") => { bool };
    ("split-inherit-working-directory") => { bool };
    ("window-inherit-font-size") => { bool };
    ("window-decoration") => { $crate::config::config::WindowDecoration };
    ("window-title-font-family") => { ::core::option::Option<$crate::config::config::StringZ> };
    ("window-subtitle") => { $crate::config::config::WindowSubtitle };
    ("window-theme") => { $crate::config::config::WindowTheme };
    ("window-colorspace") => { $crate::config::config::WindowColorspace };
    ("window-height") => { u32 };
    ("window-width") => { u32 };
    ("window-position-x") => { ::core::option::Option<i16> };
    ("window-position-y") => { ::core::option::Option<i16> };
    ("window-save-state") => { $crate::config::config::WindowSaveState };
    ("window-step-resize") => { bool };
    ("window-new-tab-position") => { $crate::config::config::WindowNewTabPosition };
    ("window-show-tab-bar") => { $crate::config::config::WindowShowTabBar };
    ("window-titlebar-background") => { ::core::option::Option<$crate::config::config::Color> };
    ("window-titlebar-foreground") => { ::core::option::Option<$crate::config::config::Color> };
    ("drag-handle") => { $crate::config::config::DragHandle };
    ("resize-overlay") => { $crate::config::config::ResizeOverlay };
    ("resize-overlay-position") => { $crate::config::config::ResizeOverlayPosition };
    ("resize-overlay-duration") => { $crate::config::config::Duration };
    ("focus-follows-mouse") => { bool };
    ("clipboard-read") => { $crate::config::config::ClipboardAccess };
    ("clipboard-write") => { $crate::config::config::ClipboardAccess };
    ("clipboard-write-limit-bytes") => { $crate::config::config::Limit<usize, { 64 * 1024 * 1024 }> };
    ("clipboard-trim-trailing-spaces") => { bool };
    ("clipboard-paste-protection") => { bool };
    ("clipboard-paste-bracketed-safe") => { bool };
    ("title-report") => { bool };
    ("image-storage-limit") => { u32 };
    ("copy-on-select") => { $crate::config::config::CopyOnSelect };
    ("right-click-action") => { $crate::config::config::RightClickAction };
    ("middle-click-action") => { $crate::config::config::MiddleClickAction };
    ("click-repeat-interval") => { u32 };
    ("config-file") => { $crate::config::config::RepeatablePath };
    ("config-default-files") => { bool };
    ("confirm-close-surface") => { $crate::config::config::ConfirmCloseSurface };
    ("quit-after-last-window-closed") => { bool };
    ("quit-after-last-window-closed-delay") => { ::core::option::Option<$crate::config::config::Duration> };
    ("initial-window") => { bool };
    ("undo-timeout") => { $crate::config::config::Duration };
    ("quick-terminal-position") => { $crate::config::config::QuickTerminalPosition };
    ("quick-terminal-size") => { $crate::config::config::QuickTerminalSize };
    ("gtk-quick-terminal-layer") => { $crate::config::config::QuickTerminalLayer };
    ("gtk-quick-terminal-namespace") => { $crate::config::config::StringZ };
    ("quick-terminal-screen") => { $crate::config::config::QuickTerminalScreen };
    ("quick-terminal-animation-duration") => { f64 };
    ("quick-terminal-autohide") => { bool };
    ("quick-terminal-space-behavior") => { $crate::config::config::QuickTerminalSpaceBehavior };
    ("quick-terminal-keyboard-interactivity") => { $crate::config::config::QuickTerminalKeyboardInteractivity };
    ("shell-integration") => { $crate::config::config::ShellIntegration };
    ("shell-integration-features") => { $crate::config::config::ShellIntegrationFeatures };
    ("command-palette-entry") => { $crate::config::config::RepeatableCommand };
    ("osc-color-report-format") => { $crate::config::config::OSCColorReportFormat };
    ("vt-kam-allowed") => { bool };
    ("vt-window-resize-allowed") => { bool };
    ("vt-xt-checksum-report") => { bool };
    ("vt-xt-checksum-extension") => { $crate::config::config::XtChecksumExtension };
    ("custom-shader") => { $crate::config::config::RepeatablePath };
    ("custom-shader-animation") => { $crate::config::config::CustomShaderAnimation };
    ("bell-features") => { $crate::config::config::BellFeatures };
    ("bell-audio-path") => { ::core::option::Option<$crate::config::config::Path> };
    ("bell-audio-volume") => { f64 };
    ("app-notifications") => { $crate::config::config::AppNotifications };
    ("macos-non-native-fullscreen") => { $crate::config::config::NonNativeFullscreen };
    ("macos-window-buttons") => { $crate::config::config::MacWindowButtons };
    ("macos-titlebar-style") => { $crate::config::config::MacTitlebarStyle };
    ("macos-titlebar-proxy-icon") => { $crate::config::config::MacTitlebarProxyIcon };
    ("macos-dock-drop-behavior") => { $crate::config::config::MacOSDockDropBehavior };
    ("macos-option-as-alt") => { ::core::option::Option<$crate::config::config::OptionAsAlt> };
    ("macos-window-shadow") => { bool };
    ("macos-hidden") => { $crate::config::config::MacHidden };
    ("macos-auto-secure-input") => { bool };
    ("macos-secure-input-indication") => { bool };
    ("macos-applescript") => { bool };
    ("macos-icon") => { $crate::config::config::MacAppIcon };
    ("macos-custom-icon") => { ::core::option::Option<$crate::config::config::StringZ> };
    ("macos-icon-frame") => { $crate::config::config::MacAppIconFrame };
    ("macos-icon-ghost-color") => { ::core::option::Option<$crate::config::config::Color> };
    ("macos-icon-screen-color") => { ::core::option::Option<$crate::config::config::ColorList> };
    ("macos-shortcuts") => { $crate::config::config::MacShortcuts };
    ("linux-cgroup") => { $crate::config::config::LinuxCgroup };
    ("linux-cgroup-memory-limit") => { ::core::option::Option<u64> };
    ("linux-cgroup-processes-limit") => { ::core::option::Option<u64> };
    ("linux-cgroup-hard-fail") => { bool };
    ("gtk-opengl-debug") => { bool };
    ("gtk-single-instance") => { $crate::config::config::GtkSingleInstance };
    ("gtk-titlebar") => { bool };
    ("gtk-tabs-location") => { $crate::config::config::GtkTabsLocation };
    ("gtk-tabs-inverted") => { bool };
    ("gtk-titlebar-hide-when-maximized") => { bool };
    ("gtk-toolbar-style") => { $crate::config::config::GtkToolbarStyle };
    ("gtk-titlebar-style") => { $crate::config::config::GtkTitlebarStyle };
    ("gtk-wide-tabs") => { bool };
    ("gtk-horizontal-tab-scroll") => { bool };
    ("gtk-custom-css") => { $crate::config::config::RepeatablePath };
    ("desktop-notifications") => { bool };
    ("progress-style") => { bool };
    ("bold-color") => { ::core::option::Option<$crate::config::config::BoldColor> };
    ("faint-opacity") => { f64 };
    ("term") => { ::std::string::String };
    ("enquiry-response") => { ::std::string::String };
    ("async-backend") => { $crate::config::config::AsyncBackend };
    ("auto-update") => { ::core::option::Option<$crate::config::config::AutoUpdate> };
    ("auto-update-channel") => { ::core::option::Option<$crate::config::config::ReleaseChannel> };
}
pub use crate::__config_key_value as Value;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Fields;
    use crate::config::config::{Config, RepeatableString};
    use std::any::TypeId;

    #[test]
    fn value() {
        assert_eq!(TypeId::of::<RepeatableString>(), TypeId::of::<Value!("font-family")>());
        assert_eq!(TypeId::of::<Option<bool>>(), TypeId::of::<Value!("cursor-style-blink")>());
    }

    #[test]
    fn key_order_matches_fields() {
        assert_eq!(<Key as CliEnum>::FIELD_NAMES.len(), Config::FIELDS.len());
        for (i, f) in Config::FIELDS.iter().enumerate() {
            assert_eq!(Key::from_index(i).tag_name(), f.name);
            assert_eq!(Key::from_index(i).index(), i);
        }
    }
}
