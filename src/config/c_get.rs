//! Port of `src/config/c_get.zig`.
//!
//! STATUS: ported
//!
//! Zig's `getValue` switches over `@TypeOf(value)` at comptime. The Rust
//! port implements [`CGetValue`] for each config field type with the same
//! rules:
//!
//! * `?[:0]const u8`: a (possibly null) `const char*`, always succeeds.
//! * `bool`: `bool`; `u8`, `u32`: `c_uint`; `i16`: `c_short`; `f32`/`f64`:
//!   the same float type.
//! * optionals: `false` if null, else the child's rule.
//! * enums: the tag name as a `const char*`.
//! * structs/unions with a `cval` decl: the `cval()` result.
//! * packed structs that fit a C int: their bits as `c_uint`.
//! * everything else: unsupported (`false`).

use std::ffi::{c_char, c_short, c_uint, c_void};

use super::config::*;
use super::key::Key;
use crate::cli::args::{CliEnum, StringZ};

/// Get a value from the config by key into the given pointer. This is
/// specifically for C-compatible APIs. If you're using Zig, just access
/// the configuration directly.
///
/// The return value is false if the given key is not supported by the
/// C API yet. This is a fixable problem so if it is important to support
/// some key, please open an issue.
///
/// # Safety
///
/// `ptr_raw` must be valid for writes of the C type the key maps to (see
/// the module docs) and suitably aligned.
pub unsafe fn get(config: &Config, k: Key, ptr_raw: *mut c_void) -> bool {
    // SAFETY: forwarded from the caller.
    unsafe { field_by_key(config, k, ptr_raw) }
}

/// Get the value and put it into the pointer. Returns false if the type is
/// not supported by the C API yet or the value is null (Zig `getValue`).
pub trait CGetValue {
    /// # Safety
    ///
    /// `ptr_raw` must be valid for writes of the value's C type.
    unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool;
}

/// Zig `getValue(ptr_raw, value)`.
///
/// # Safety
///
/// See [`CGetValue::get_value`].
unsafe fn get_value<T: CGetValue + ?Sized>(ptr_raw: *mut c_void, value: &T) -> bool {
    // SAFETY: forwarded from the caller.
    unsafe { value.get_value(ptr_raw) }
}

impl CGetValue for Option<StringZ> {
    unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
        let ptr = ptr_raw as *mut *const c_char;
        // SAFETY: the caller guarantees ptr is valid for a `const char*`.
        unsafe {
            *ptr = match self {
                Some(slice) => slice.as_ptr(),
                None => std::ptr::null(),
            };
        }
        true
    }
}

impl CGetValue for bool {
    unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
        // SAFETY: the caller guarantees ptr is valid for a `bool`.
        unsafe { *(ptr_raw as *mut bool) = *self };
        true
    }
}

macro_rules! cget_uint {
    ($($t:ty),*) => {$(
        impl CGetValue for $t {
            unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
                // SAFETY: the caller guarantees ptr is valid for a `c_uint`.
                unsafe { *(ptr_raw as *mut c_uint) = *self as c_uint };
                true
            }
        }
    )*};
}
cget_uint!(u8, u32);

impl CGetValue for i16 {
    unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
        // SAFETY: the caller guarantees ptr is valid for a `c_short`.
        unsafe { *(ptr_raw as *mut c_short) = *self as c_short };
        true
    }
}

macro_rules! cget_float {
    ($($t:ty),*) => {$(
        impl CGetValue for $t {
            unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
                // SAFETY: the caller guarantees ptr is valid for this float.
                unsafe { *(ptr_raw as *mut $t) = *self };
                true
            }
        }
    )*};
}
cget_float!(f32, f64);

/// Types that are not supported by the C API (yet).
macro_rules! cget_unsupported {
    ($($t:ty),* $(,)?) => {$(
        impl CGetValue for $t {
            unsafe fn get_value(&self, _ptr_raw: *mut c_void) -> bool {
                false
            }
        }
    )*};
}
cget_unsupported!(
    // Non-optional strings and integers that aren't handled.
    StringZ,
    String,
    u64,
    // Structs without `cval` that aren't packed.
    RepeatableString,
    RepeatablePath,
    RepeatableStringMap,
    RepeatableReadableIO,
    RepeatableFontVariation,
    RepeatableCodepointMap,
    RepeatableClipboardCodepointMap,
    RepeatableLink,
    SelectionWordChars,
    Keybinds,
    KeyRemapSet,
    WindowPadding,
    MouseScrollMultiplier,
    Theme,
    Limit<usize, 50_000_000>,
    Limit<usize, { usize::MAX as i128 }>,
    Limit<usize, { 64 * 1024 * 1024 }>,
    // Unions without `cval`.
    FontStyle,
    TerminalColor,
    BoldColor,
    WorkingDirectory,
    Command,
    MetricModifier,
);

/// Optionals: if an optional has no value we return false.
macro_rules! cget_option {
    ($($t:ty),* $(,)?) => {$(
        impl CGetValue for Option<$t> {
            unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
                match self {
                    // SAFETY: forwarded from the caller.
                    Some(unwrapped) => unsafe { get_value(ptr_raw, unwrapped) },
                    None => false,
                }
            }
        }
    )*};
}
cget_option!(
    bool,
    i16,
    u64,
    Color,
    ColorList,
    TerminalColor,
    BoldColor,
    Duration,
    Path,
    Theme,
    Command,
    WorkingDirectory,
    MetricModifier,
    OptionAsAlt,
    AutoUpdate,
    ReleaseChannel,
);

/// Enums: the tag name as a `const char*`.
macro_rules! cget_enum {
    ($($t:ty),* $(,)?) => {$(
        impl CGetValue for $t {
            unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
                let ptr = ptr_raw as *mut *const c_char;
                // SAFETY: the caller guarantees ptr is valid for a `const char*`.
                unsafe { *ptr = CliEnum::tag_name_c(*self).as_ptr() };
                true
            }
        }
    )*};
}
cget_enum!(
    ConfirmCloseSurface,
    CustomShaderAnimation,
    NonNativeFullscreen,
    Fullscreen,
    WindowPaddingColor,
    WindowSubtitle,
    LinkPreviews,
    CopyOnSelect,
    RightClickAction,
    MiddleClickAction,
    ShellIntegration,
    OSCColorReportFormat,
    WindowTheme,
    WindowColorspace,
    MacWindowButtons,
    MacTitlebarStyle,
    MacTitlebarProxyIcon,
    MacHidden,
    MacAppIcon,
    MacAppIconFrame,
    MacShortcuts,
    GtkSingleInstance,
    GtkTabsLocation,
    GtkToolbarStyle,
    GtkTitlebarStyle,
    MouseShiftCapture,
    ClipboardAccess,
    WindowSaveState,
    WindowNewTabPosition,
    MacOSDockDropBehavior,
    WindowShowTabBar,
    DragHandle,
    ResizeOverlay,
    ResizeOverlayPosition,
    QuickTerminalPosition,
    QuickTerminalLayer,
    QuickTerminalScreen,
    QuickTerminalSpaceBehavior,
    QuickTerminalKeyboardInteractivity,
    GraphemeWidthMethod,
    AlphaBlending,
    BackgroundImagePosition,
    BackgroundImageFit,
    LinuxCgroup,
    AsyncBackend,
    AutoUpdate,
    WindowDecoration,
    Scrollbar,
    NotifyOnCommandFinish,
    CursorStyle,
    OptionAsAlt,
    ReleaseChannel,
    WindowPaddingBalance,
);

/// Structs and unions with a `cval` decl: the `cval()` result.
macro_rules! cget_cval {
    ($($t:ty => $c:ty),* $(,)?) => {$(
        impl CGetValue for $t {
            unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
                // SAFETY: the caller guarantees ptr is valid for the cval type.
                unsafe { *(ptr_raw as *mut $c) = self.cval() };
                true
            }
        }
    )*};
}
cget_cval!(
    Color => ColorC,
    ColorList => ColorListC,
    Palette => PaletteC,
    QuickTerminalSize => QuickTerminalSizeC,
    RepeatableCommand => RepeatableCommandC,
    Duration => usize,
    BackgroundBlur => i16,
    Path => super::path::C,
);

/// Packed structs that are less than or equal to the size of a C int can
/// be passed directly as their bit representation.
macro_rules! cget_packed {
    ($($t:ty),* $(,)?) => {$(
        impl CGetValue for $t {
            unsafe fn get_value(&self, ptr_raw: *mut c_void) -> bool {
                if <$t>::BITS > c_uint::BITS {
                    return false;
                }
                // SAFETY: the caller guarantees ptr is valid for a `c_uint`.
                unsafe { *(ptr_raw as *mut c_uint) = self.to_bits() as c_uint };
                true
            }
        }
    )*};
}
cget_packed!(
    FontSyntheticStyle,
    FontShapingBreak,
    FreetypeLoadFlags,
    ShellIntegrationFeatures,
    XtChecksumExtension,
    SplitPreserveZoom,
    AppNotifications,
    BellFeatures,
    ScrollToBottom,
    NotifyOnCommandFinishAction,
);

/// Get a value from the config by key.
///
/// # Safety
///
/// See [`get`].
unsafe fn field_by_key(config: &Config, k: Key, ptr_raw: *mut c_void) -> bool {
    // SAFETY: forwarded from the caller.
    unsafe {
        match k {
            Key::Language => get_value(ptr_raw, &config.language),
            Key::FontFamily => get_value(ptr_raw, &config.font_family),
            Key::FontFamilyBold => get_value(ptr_raw, &config.font_family_bold),
            Key::FontFamilyItalic => get_value(ptr_raw, &config.font_family_italic),
            Key::FontFamilyBoldItalic => get_value(ptr_raw, &config.font_family_bold_italic),
            Key::FontStyle => get_value(ptr_raw, &config.font_style),
            Key::FontStyleBold => get_value(ptr_raw, &config.font_style_bold),
            Key::FontStyleItalic => get_value(ptr_raw, &config.font_style_italic),
            Key::FontStyleBoldItalic => get_value(ptr_raw, &config.font_style_bold_italic),
            Key::FontSyntheticStyle => get_value(ptr_raw, &config.font_synthetic_style),
            Key::FontFeature => get_value(ptr_raw, &config.font_feature),
            Key::FontSize => get_value(ptr_raw, &config.font_size),
            Key::FontVariation => get_value(ptr_raw, &config.font_variation),
            Key::FontVariationBold => get_value(ptr_raw, &config.font_variation_bold),
            Key::FontVariationItalic => get_value(ptr_raw, &config.font_variation_italic),
            Key::FontVariationBoldItalic => get_value(ptr_raw, &config.font_variation_bold_italic),
            Key::FontCodepointMap => get_value(ptr_raw, &config.font_codepoint_map),
            Key::ClipboardCodepointMap => get_value(ptr_raw, &config.clipboard_codepoint_map),
            Key::FontThicken => get_value(ptr_raw, &config.font_thicken),
            Key::FontThickenStrength => get_value(ptr_raw, &config.font_thicken_strength),
            Key::FontShapingBreak => get_value(ptr_raw, &config.font_shaping_break),
            Key::AlphaBlending => get_value(ptr_raw, &config.alpha_blending),
            Key::AdjustCellWidth => get_value(ptr_raw, &config.adjust_cell_width),
            Key::AdjustCellHeight => get_value(ptr_raw, &config.adjust_cell_height),
            Key::AdjustFontBaseline => get_value(ptr_raw, &config.adjust_font_baseline),
            Key::AdjustUnderlinePosition => get_value(ptr_raw, &config.adjust_underline_position),
            Key::AdjustUnderlineThickness => get_value(ptr_raw, &config.adjust_underline_thickness),
            Key::AdjustStrikethroughPosition => get_value(ptr_raw, &config.adjust_strikethrough_position),
            Key::AdjustStrikethroughThickness => get_value(ptr_raw, &config.adjust_strikethrough_thickness),
            Key::AdjustOverlinePosition => get_value(ptr_raw, &config.adjust_overline_position),
            Key::AdjustOverlineThickness => get_value(ptr_raw, &config.adjust_overline_thickness),
            Key::AdjustCursorThickness => get_value(ptr_raw, &config.adjust_cursor_thickness),
            Key::AdjustCursorHeight => get_value(ptr_raw, &config.adjust_cursor_height),
            Key::AdjustBoxThickness => get_value(ptr_raw, &config.adjust_box_thickness),
            Key::AdjustIconHeight => get_value(ptr_raw, &config.adjust_icon_height),
            Key::GraphemeWidthMethod => get_value(ptr_raw, &config.grapheme_width_method),
            Key::FreetypeLoadFlags => get_value(ptr_raw, &config.freetype_load_flags),
            Key::Theme => get_value(ptr_raw, &config.theme),
            Key::Background => get_value(ptr_raw, &config.background),
            Key::Foreground => get_value(ptr_raw, &config.foreground),
            Key::BackgroundImage => get_value(ptr_raw, &config.background_image),
            Key::BackgroundImageOpacity => get_value(ptr_raw, &config.background_image_opacity),
            Key::BackgroundImagePosition => get_value(ptr_raw, &config.background_image_position),
            Key::BackgroundImageFit => get_value(ptr_raw, &config.background_image_fit),
            Key::BackgroundImageRepeat => get_value(ptr_raw, &config.background_image_repeat),
            Key::SelectionForeground => get_value(ptr_raw, &config.selection_foreground),
            Key::SelectionBackground => get_value(ptr_raw, &config.selection_background),
            Key::SelectionClearOnTyping => get_value(ptr_raw, &config.selection_clear_on_typing),
            Key::SelectionClearOnCopy => get_value(ptr_raw, &config.selection_clear_on_copy),
            Key::SelectionWordChars => get_value(ptr_raw, &config.selection_word_chars),
            Key::MinimumContrast => get_value(ptr_raw, &config.minimum_contrast),
            Key::Palette => get_value(ptr_raw, &config.palette),
            Key::PaletteGenerate => get_value(ptr_raw, &config.palette_generate),
            Key::PaletteHarmonious => get_value(ptr_raw, &config.palette_harmonious),
            Key::CursorColor => get_value(ptr_raw, &config.cursor_color),
            Key::CursorOpacity => get_value(ptr_raw, &config.cursor_opacity),
            Key::CursorStyle => get_value(ptr_raw, &config.cursor_style),
            Key::CursorStyleBlink => get_value(ptr_raw, &config.cursor_style_blink),
            Key::CursorText => get_value(ptr_raw, &config.cursor_text),
            Key::CursorClickToMove => get_value(ptr_raw, &config.cursor_click_to_move),
            Key::MouseHideWhileTyping => get_value(ptr_raw, &config.mouse_hide_while_typing),
            Key::ScrollToBottom => get_value(ptr_raw, &config.scroll_to_bottom),
            Key::MouseShiftCapture => get_value(ptr_raw, &config.mouse_shift_capture),
            Key::MouseReporting => get_value(ptr_raw, &config.mouse_reporting),
            Key::MouseScrollMultiplier => get_value(ptr_raw, &config.mouse_scroll_multiplier),
            Key::BackgroundOpacity => get_value(ptr_raw, &config.background_opacity),
            Key::BackgroundOpacityCells => get_value(ptr_raw, &config.background_opacity_cells),
            Key::BackgroundBlur => get_value(ptr_raw, &config.background_blur),
            Key::UnfocusedSplitOpacity => get_value(ptr_raw, &config.unfocused_split_opacity),
            Key::UnfocusedSplitFill => get_value(ptr_raw, &config.unfocused_split_fill),
            Key::SplitDividerColor => get_value(ptr_raw, &config.split_divider_color),
            Key::SplitPreserveZoom => get_value(ptr_raw, &config.split_preserve_zoom),
            Key::SearchForeground => get_value(ptr_raw, &config.search_foreground),
            Key::SearchBackground => get_value(ptr_raw, &config.search_background),
            Key::SearchSelectedForeground => get_value(ptr_raw, &config.search_selected_foreground),
            Key::SearchSelectedBackground => get_value(ptr_raw, &config.search_selected_background),
            Key::Command => get_value(ptr_raw, &config.command),
            Key::InitialCommand => get_value(ptr_raw, &config.initial_command),
            Key::NotifyOnCommandFinish => get_value(ptr_raw, &config.notify_on_command_finish),
            Key::NotifyOnCommandFinishAction => get_value(ptr_raw, &config.notify_on_command_finish_action),
            Key::NotifyOnCommandFinishAfter => get_value(ptr_raw, &config.notify_on_command_finish_after),
            Key::Env => get_value(ptr_raw, &config.env),
            Key::Input => get_value(ptr_raw, &config.input),
            Key::WaitAfterCommand => get_value(ptr_raw, &config.wait_after_command),
            Key::AbnormalCommandExitRuntime => get_value(ptr_raw, &config.abnormal_command_exit_runtime),
            Key::ScrollbackLimitBytes => get_value(ptr_raw, &config.scrollback_limit_bytes),
            Key::ScrollbackLimitLines => get_value(ptr_raw, &config.scrollback_limit_lines),
            Key::ScrollbackCompression => get_value(ptr_raw, &config.scrollback_compression),
            Key::Scrollbar => get_value(ptr_raw, &config.scrollbar),
            Key::Link => get_value(ptr_raw, &config.link),
            Key::LinkUrl => get_value(ptr_raw, &config.link_url),
            Key::LinkOsc8 => get_value(ptr_raw, &config.link_osc8),
            Key::LinkPreviews => get_value(ptr_raw, &config.link_previews),
            Key::Maximize => get_value(ptr_raw, &config.maximize),
            Key::Fullscreen => get_value(ptr_raw, &config.fullscreen),
            Key::Title => get_value(ptr_raw, &config.title),
            Key::Class => get_value(ptr_raw, &config.class),
            Key::X11InstanceName => get_value(ptr_raw, &config.x11_instance_name),
            Key::WorkingDirectory => get_value(ptr_raw, &config.working_directory),
            Key::Keybind => get_value(ptr_raw, &config.keybind),
            Key::KeyRemap => get_value(ptr_raw, &config.key_remap),
            Key::WindowPaddingX => get_value(ptr_raw, &config.window_padding_x),
            Key::WindowPaddingY => get_value(ptr_raw, &config.window_padding_y),
            Key::WindowPaddingBalance => get_value(ptr_raw, &config.window_padding_balance),
            Key::WindowPaddingColor => get_value(ptr_raw, &config.window_padding_color),
            Key::WindowVsync => get_value(ptr_raw, &config.window_vsync),
            Key::WindowInheritWorkingDirectory => get_value(ptr_raw, &config.window_inherit_working_directory),
            Key::TabInheritWorkingDirectory => get_value(ptr_raw, &config.tab_inherit_working_directory),
            Key::SplitInheritWorkingDirectory => get_value(ptr_raw, &config.split_inherit_working_directory),
            Key::WindowInheritFontSize => get_value(ptr_raw, &config.window_inherit_font_size),
            Key::WindowDecoration => get_value(ptr_raw, &config.window_decoration),
            Key::WindowTitleFontFamily => get_value(ptr_raw, &config.window_title_font_family),
            Key::WindowSubtitle => get_value(ptr_raw, &config.window_subtitle),
            Key::WindowTheme => get_value(ptr_raw, &config.window_theme),
            Key::WindowColorspace => get_value(ptr_raw, &config.window_colorspace),
            Key::WindowHeight => get_value(ptr_raw, &config.window_height),
            Key::WindowWidth => get_value(ptr_raw, &config.window_width),
            Key::WindowPositionX => get_value(ptr_raw, &config.window_position_x),
            Key::WindowPositionY => get_value(ptr_raw, &config.window_position_y),
            Key::WindowSaveState => get_value(ptr_raw, &config.window_save_state),
            Key::WindowStepResize => get_value(ptr_raw, &config.window_step_resize),
            Key::WindowNewTabPosition => get_value(ptr_raw, &config.window_new_tab_position),
            Key::WindowShowTabBar => get_value(ptr_raw, &config.window_show_tab_bar),
            Key::WindowTitlebarBackground => get_value(ptr_raw, &config.window_titlebar_background),
            Key::WindowTitlebarForeground => get_value(ptr_raw, &config.window_titlebar_foreground),
            Key::DragHandle => get_value(ptr_raw, &config.drag_handle),
            Key::ResizeOverlay => get_value(ptr_raw, &config.resize_overlay),
            Key::ResizeOverlayPosition => get_value(ptr_raw, &config.resize_overlay_position),
            Key::ResizeOverlayDuration => get_value(ptr_raw, &config.resize_overlay_duration),
            Key::FocusFollowsMouse => get_value(ptr_raw, &config.focus_follows_mouse),
            Key::ClipboardRead => get_value(ptr_raw, &config.clipboard_read),
            Key::ClipboardWrite => get_value(ptr_raw, &config.clipboard_write),
            Key::ClipboardWriteLimitBytes => get_value(ptr_raw, &config.clipboard_write_limit_bytes),
            Key::ClipboardTrimTrailingSpaces => get_value(ptr_raw, &config.clipboard_trim_trailing_spaces),
            Key::ClipboardPasteProtection => get_value(ptr_raw, &config.clipboard_paste_protection),
            Key::ClipboardPasteBracketedSafe => get_value(ptr_raw, &config.clipboard_paste_bracketed_safe),
            Key::TitleReport => get_value(ptr_raw, &config.title_report),
            Key::ImageStorageLimit => get_value(ptr_raw, &config.image_storage_limit),
            Key::CopyOnSelect => get_value(ptr_raw, &config.copy_on_select),
            Key::RightClickAction => get_value(ptr_raw, &config.right_click_action),
            Key::MiddleClickAction => get_value(ptr_raw, &config.middle_click_action),
            Key::ClickRepeatInterval => get_value(ptr_raw, &config.click_repeat_interval),
            Key::ConfigFile => get_value(ptr_raw, &config.config_file),
            Key::ConfigDefaultFiles => get_value(ptr_raw, &config.config_default_files),
            Key::ConfirmCloseSurface => get_value(ptr_raw, &config.confirm_close_surface),
            Key::QuitAfterLastWindowClosed => get_value(ptr_raw, &config.quit_after_last_window_closed),
            Key::QuitAfterLastWindowClosedDelay => get_value(ptr_raw, &config.quit_after_last_window_closed_delay),
            Key::InitialWindow => get_value(ptr_raw, &config.initial_window),
            Key::UndoTimeout => get_value(ptr_raw, &config.undo_timeout),
            Key::QuickTerminalPosition => get_value(ptr_raw, &config.quick_terminal_position),
            Key::QuickTerminalSize => get_value(ptr_raw, &config.quick_terminal_size),
            Key::GtkQuickTerminalLayer => get_value(ptr_raw, &config.gtk_quick_terminal_layer),
            Key::GtkQuickTerminalNamespace => get_value(ptr_raw, &config.gtk_quick_terminal_namespace),
            Key::QuickTerminalScreen => get_value(ptr_raw, &config.quick_terminal_screen),
            Key::QuickTerminalAnimationDuration => get_value(ptr_raw, &config.quick_terminal_animation_duration),
            Key::QuickTerminalAutohide => get_value(ptr_raw, &config.quick_terminal_autohide),
            Key::QuickTerminalSpaceBehavior => get_value(ptr_raw, &config.quick_terminal_space_behavior),
            Key::QuickTerminalKeyboardInteractivity => get_value(ptr_raw, &config.quick_terminal_keyboard_interactivity),
            Key::ShellIntegration => get_value(ptr_raw, &config.shell_integration),
            Key::ShellIntegrationFeatures => get_value(ptr_raw, &config.shell_integration_features),
            Key::CommandPaletteEntry => get_value(ptr_raw, &config.command_palette_entry),
            Key::OscColorReportFormat => get_value(ptr_raw, &config.osc_color_report_format),
            Key::VtKamAllowed => get_value(ptr_raw, &config.vt_kam_allowed),
            Key::VtWindowResizeAllowed => get_value(ptr_raw, &config.vt_window_resize_allowed),
            Key::VtXtChecksumReport => get_value(ptr_raw, &config.vt_xt_checksum_report),
            Key::VtXtChecksumExtension => get_value(ptr_raw, &config.vt_xt_checksum_extension),
            Key::CustomShader => get_value(ptr_raw, &config.custom_shader),
            Key::CustomShaderAnimation => get_value(ptr_raw, &config.custom_shader_animation),
            Key::BellFeatures => get_value(ptr_raw, &config.bell_features),
            Key::BellAudioPath => get_value(ptr_raw, &config.bell_audio_path),
            Key::BellAudioVolume => get_value(ptr_raw, &config.bell_audio_volume),
            Key::AppNotifications => get_value(ptr_raw, &config.app_notifications),
            Key::MacosNonNativeFullscreen => get_value(ptr_raw, &config.macos_non_native_fullscreen),
            Key::MacosWindowButtons => get_value(ptr_raw, &config.macos_window_buttons),
            Key::MacosTitlebarStyle => get_value(ptr_raw, &config.macos_titlebar_style),
            Key::MacosTitlebarProxyIcon => get_value(ptr_raw, &config.macos_titlebar_proxy_icon),
            Key::MacosDockDropBehavior => get_value(ptr_raw, &config.macos_dock_drop_behavior),
            Key::MacosOptionAsAlt => get_value(ptr_raw, &config.macos_option_as_alt),
            Key::MacosWindowShadow => get_value(ptr_raw, &config.macos_window_shadow),
            Key::MacosHidden => get_value(ptr_raw, &config.macos_hidden),
            Key::MacosAutoSecureInput => get_value(ptr_raw, &config.macos_auto_secure_input),
            Key::MacosSecureInputIndication => get_value(ptr_raw, &config.macos_secure_input_indication),
            Key::MacosApplescript => get_value(ptr_raw, &config.macos_applescript),
            Key::MacosIcon => get_value(ptr_raw, &config.macos_icon),
            Key::MacosCustomIcon => get_value(ptr_raw, &config.macos_custom_icon),
            Key::MacosIconFrame => get_value(ptr_raw, &config.macos_icon_frame),
            Key::MacosIconGhostColor => get_value(ptr_raw, &config.macos_icon_ghost_color),
            Key::MacosIconScreenColor => get_value(ptr_raw, &config.macos_icon_screen_color),
            Key::MacosShortcuts => get_value(ptr_raw, &config.macos_shortcuts),
            Key::LinuxCgroup => get_value(ptr_raw, &config.linux_cgroup),
            Key::LinuxCgroupMemoryLimit => get_value(ptr_raw, &config.linux_cgroup_memory_limit),
            Key::LinuxCgroupProcessesLimit => get_value(ptr_raw, &config.linux_cgroup_processes_limit),
            Key::LinuxCgroupHardFail => get_value(ptr_raw, &config.linux_cgroup_hard_fail),
            Key::GtkOpenglDebug => get_value(ptr_raw, &config.gtk_opengl_debug),
            Key::GtkSingleInstance => get_value(ptr_raw, &config.gtk_single_instance),
            Key::GtkTitlebar => get_value(ptr_raw, &config.gtk_titlebar),
            Key::GtkTabsLocation => get_value(ptr_raw, &config.gtk_tabs_location),
            Key::GtkTabsInverted => get_value(ptr_raw, &config.gtk_tabs_inverted),
            Key::GtkTitlebarHideWhenMaximized => get_value(ptr_raw, &config.gtk_titlebar_hide_when_maximized),
            Key::GtkToolbarStyle => get_value(ptr_raw, &config.gtk_toolbar_style),
            Key::GtkTitlebarStyle => get_value(ptr_raw, &config.gtk_titlebar_style),
            Key::GtkWideTabs => get_value(ptr_raw, &config.gtk_wide_tabs),
            Key::GtkHorizontalTabScroll => get_value(ptr_raw, &config.gtk_horizontal_tab_scroll),
            Key::GtkCustomCss => get_value(ptr_raw, &config.gtk_custom_css),
            Key::DesktopNotifications => get_value(ptr_raw, &config.desktop_notifications),
            Key::ProgressStyle => get_value(ptr_raw, &config.progress_style),
            Key::BoldColor => get_value(ptr_raw, &config.bold_color),
            Key::FaintOpacity => get_value(ptr_raw, &config.faint_opacity),
            Key::Term => get_value(ptr_raw, &config.term),
            Key::EnquiryResponse => get_value(ptr_raw, &config.enquiry_response),
            Key::AsyncBackend => get_value(ptr_raw, &config.async_backend),
            Key::AutoUpdate => get_value(ptr_raw, &config.auto_update),
            Key::AutoUpdateChannel => get_value(ptr_raw, &config.auto_update_channel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn c_get_u8() {
        let mut c = Config::default();
        c.font_size = 24.0;

        let mut cval: f32 = 0.0;
        assert!(unsafe { get(&c, Key::FontSize, &mut cval as *mut f32 as *mut c_void) });
        assert_eq!(24.0f32, cval);
    }

    #[test]
    fn c_get_enum() {
        let mut c = Config::default();
        c.window_theme = WindowTheme::Dark;

        let mut cval: *const c_char = std::ptr::null();
        assert!(unsafe { get(&c, Key::WindowTheme, &mut cval as *mut _ as *mut c_void) });

        let str = unsafe { CStr::from_ptr(cval) };
        assert_eq!("dark", str.to_str().unwrap());
    }

    #[test]
    fn c_get_color() {
        let mut c = Config::default();
        c.background = Color { r: 255, g: 0, b: 0 };

        let mut cval = ColorC::default();
        assert!(unsafe { get(&c, Key::Background, &mut cval as *mut _ as *mut c_void) });
        assert_eq!(255, cval.r);
        assert_eq!(0, cval.g);
        assert_eq!(0, cval.b);
    }

    #[test]
    fn c_get_optional() {
        let mut c = Config::default();

        {
            c.unfocused_split_fill = None;
            let mut cval = ColorC::default();
            assert!(!unsafe { get(&c, Key::UnfocusedSplitFill, &mut cval as *mut _ as *mut c_void) });
        }

        {
            c.unfocused_split_fill = Some(Color { r: 255, g: 0, b: 0 });
            let mut cval = ColorC::default();
            assert!(unsafe { get(&c, Key::UnfocusedSplitFill, &mut cval as *mut _ as *mut c_void) });
            assert_eq!(255, cval.r);
            assert_eq!(0, cval.g);
            assert_eq!(0, cval.b);
        }
    }

    #[test]
    fn c_get_background_blur() {
        let mut c = Config::default();

        let cases = [
            (BackgroundBlur::False, 0i16),
            (BackgroundBlur::True, 20),
            (BackgroundBlur::Radius(42), 42),
            (BackgroundBlur::MacosGlassRegular, -1),
            (BackgroundBlur::MacosGlassClear, -2),
        ];
        for (blur, expected) in cases {
            c.background_blur = blur;
            let mut cval: i16 = 0;
            assert!(unsafe { get(&c, Key::BackgroundBlur, &mut cval as *mut _ as *mut c_void) });
            assert_eq!(expected, cval);
        }
    }

    #[test]
    fn c_get_split_preserve_zoom() {
        let mut c = Config::default();

        let mut bits: c_uint = 0xFFFF;
        assert!(unsafe { get(&c, Key::SplitPreserveZoom, &mut bits as *mut _ as *mut c_void) });
        assert_eq!(0 as c_uint, bits);

        c.split_preserve_zoom.navigation = true;
        assert!(unsafe { get(&c, Key::SplitPreserveZoom, &mut bits as *mut _ as *mut c_void) });
        assert_eq!(1 as c_uint, bits);
    }
}
