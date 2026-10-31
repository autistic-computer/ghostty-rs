//! Port of `src/apprt/action.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * Payloads that borrow strings (`[:0]const u8` / `[]const u8` in Zig)
//!   borrow them in Rust too, so [`Action`] has a lifetime. NUL-terminated
//!   strings that cross the C ABI as `[*:0]const u8` are `&CStr`.
//! * Zig computes `Action.CValue` at comptime from the payload types; the
//!   Rust port generates the enum, its C tag enum, the `#[repr(C)]` value
//!   union and `cval()` from one list with a local macro. `Action.Value(key)`
//!   (a comptime type function) has no Rust counterpart: use the variant's
//!   payload type directly.
//! * Non-exhaustive C enums (`GotoTab`, `ColorKind`) are transparent
//!   `c_int` newtypes with associated constants for the named values.
//! * `InitialSize.getGObjectType` (GObject boxed registration) is skipped;
//!   the GTK apprt registers its own GObject types.
//! * Zig's `format` methods are `Display` impls.
//! * [`Target::Surface`] holds a raw pointer like Zig's `*CoreSurface`.

use std::ffi::{c_char, c_int, CStr};
use std::fmt;

use crate::apprt;
use crate::apprt::surface::ChildExited;
use crate::input::binding::{Trigger, TriggerC};
use crate::renderer::Health;
use crate::terminal::mouse::Shape as MouseShape;
use crate::terminal::osc::{ProgressReport, ProgressReportC};

use crate::config::config::Duration as ConfigDuration;
use crate::config::Config;

use crate::surface::Surface as CoreSurface;
use crate::terminal::page_list::{Scrollbar, ScrollbarC};

/// The target for an action. This is generally the thing that had focus
/// while the action was made but the concept of "focus" is not guaranteed
/// since actions can also be triggered by timers, scripts, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    App,
    Surface(*mut CoreSurface),
}

crate::lib_enum! {
    // Sync with: ghostty_target_tag_e
    pub enum TargetKey: c {
        App,
        Surface,
    }
}

// Sync with: ghostty_target_u
#[repr(C)]
#[derive(Clone, Copy)]
pub union TargetCValue {
    pub app: (),
    pub surface: *mut apprt::Surface,
}

// Sync with: ghostty_target_s
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TargetC {
    pub key: TargetKey,
    pub value: TargetCValue,
}

impl Target {
    /// The active tag.
    pub fn key(&self) -> TargetKey {
        match self {
            Target::App => TargetKey::App,
            Target::Surface(_) => TargetKey::Surface,
        }
    }

    /// Convert to ghostty_target_s.
    pub fn cval(&self) -> TargetC {
        TargetC {
            key: self.key(),
            value: match self {
                Target::App => TargetCValue { app: () },
                Target::Surface(v) => TargetCValue {
                    // SAFETY: a surface target is only constructed from a
                    // live core surface, which outlives the action.
                    surface: unsafe { (**v).rt_surface },
                },
            },
        }
    }
}

/// Conversion of an action payload to its C ABI value. Zig: a payload
/// type's optional `C` decl and `cval` method (otherwise the value itself).
trait CVal {
    type C: Copy;
    fn cval(&self) -> Self::C;
}

/// Payload types that are already C ABI compatible.
macro_rules! cval_identity {
    ($($t:ty),* $(,)?) => {
        $(impl CVal for $t {
            type C = $t;
            fn cval(&self) -> $t {
                *self
            }
        })*
    };
}

/// Payload types with their own `cval` method.
macro_rules! cval_forward {
    ($($t:ty => $c:ty),* $(,)?) => {
        $(impl CVal for $t {
            type C = $c;
            fn cval(&self) -> $c {
                <$t>::cval(self)
            }
        })*
    };
}

/// Generates [`Action`], [`ActionKey`], [`ActionCValue`], [`ActionC`] and
/// the tag/C conversions from a single list of
/// `Variant(PayloadType => CType) : c_field`.
macro_rules! actions {
    (
        $lt:lifetime;
        $(
            $(#[$vmeta:meta])*
            $variant:ident $(($ty:ty => $cty:ty))? : $field:ident
        ),* $(,)?
    ) => {
        /// The possible actions an apprt has to react to. Actions are one-way
        /// messages that are sent to the app runtime to trigger some behavior.
        ///
        /// Actions are very often key binding actions but can also be triggered
        /// by lifecycle events. For example, the `quit_timer` action is not bindable.
        ///
        /// Importantly, actions are generally OPTIONAL to implement by an apprt.
        /// Required functionality is called directly on the runtime structure so
        /// there is a compiler error if an action is not implemented.
        #[derive(Debug, Clone, Copy)]
        pub enum Action<$lt> {
            $($(#[$vmeta])* $variant $(($ty))?,)*
        }

        crate::lib_enum! {
            /// Sync with: ghostty_action_tag_e
            pub enum ActionKey: c {
                $($variant,)*
            }
        }

        /// Sync with: ghostty_action_u
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub union ActionCValue {
            $(pub $field: actions!(@cty $($cty)?),)*
        }

        impl<$lt> Action<$lt> {
            /// The active tag.
            pub fn key(&self) -> ActionKey {
                match self {
                    $(actions!(@pat $variant [_v] $($ty)?) => ActionKey::$variant,)*
                }
            }

            /// Convert to ghostty_action_s.
            pub fn cval(&self) -> ActionC {
                // SAFETY: the C value union is plain old data; an all-zero
                // bit pattern is valid for every field and keeps padding
                // bytes deterministic.
                let mut value: ActionCValue = unsafe { std::mem::zeroed() };
                match self {
                    $(actions!(@pat $variant [v] $($ty)?) => {
                        value.$field = actions!(@conv [v] $($ty)?);
                    })*
                }

                ActionC {
                    key: self.key(),
                    value,
                }
            }
        }
    };

    (@cty) => { () };
    (@cty $cty:ty) => { $cty };
    (@pat $variant:ident [$v:ident]) => { Action::$variant };
    (@pat $variant:ident [$v:ident] $ty:ty) => { Action::$variant($v) };
    (@conv [$v:ident]) => { () };
    (@conv [$v:ident] $ty:ty) => { CVal::cval($v) };
}

// A GUIDE TO ADDING NEW ACTIONS:
//
// 1. Add the action to the `Key` enum. The order of the enum matters
//    because it maps directly to the libghostty C enum. For ABI
//    compatibility, new actions should be added to the end of the enum.
//
// 2. Add the action and optional value to the Action union.
//
// 3. If the value type is not void, ensure the value is C ABI
//    compatible (extern). If it is not, add a `C` decl to the value
//    and a `cval` function to convert to the C ABI compatible value.
//
// 4. Update `include/ghostty.h`: add the new key, value, and union
//    entry. If the value type is void then only the key needs to be
//    added. Ensure the order matches exactly with the Zig code.
//
// (In the Rust port, steps 1-2 are a single entry in the list below.)
actions! {
    'a;

    /// Quit the application.
    Quit: quit,

    /// Open a new window. The target determines whether properties such
    /// as font size should be inherited.
    NewWindow: new_window,

    /// Open a new tab. If the target is a surface it should be opened in
    /// the same window as the surface. If the target is the app then
    /// the tab should be opened in a new window.
    NewTab: new_tab,

    /// Closes the tab belonging to the currently focused split, or all other
    /// tabs, depending on the mode.
    CloseTab(CloseTabMode => CloseTabMode): close_tab,

    /// Create a new split. The value determines the location of the split
    /// relative to the target.
    NewSplit(SplitDirection => SplitDirection): new_split,

    /// Close all open windows.
    CloseAllWindows: close_all_windows,

    /// Toggle maximized window state.
    ToggleMaximize: toggle_maximize,

    /// Toggle fullscreen mode.
    ToggleFullscreen(Fullscreen => Fullscreen): toggle_fullscreen,

    /// Toggle tab overview.
    ToggleTabOverview: toggle_tab_overview,

    /// Toggle whether window directions are shown.
    ToggleWindowDecorations: toggle_window_decorations,

    /// Toggle the quick terminal in or out.
    ToggleQuickTerminal: toggle_quick_terminal,

    /// Toggle the command palette.
    ToggleCommandPalette: toggle_command_palette,

    /// Toggle the visibility of all Ghostty terminal windows.
    ToggleVisibility: toggle_visibility,

    /// Toggle the window background opacity. This only has an effect
    /// if the window started as transparent (non-opaque), and toggles
    /// it between fully opaque and the configured background opacity.
    ToggleBackgroundOpacity: toggle_background_opacity,

    /// Moves a tab by a relative offset.
    ///
    /// Adjusts the tab position based on `offset` (e.g., -1 for left, +1
    /// for right). If the new position is out of bounds, it wraps around
    /// cyclically within the tab range.
    MoveTab(MoveTab => MoveTab): move_tab,

    /// Jump to a specific tab. Must handle the scenario that the tab
    /// value is invalid.
    GotoTab(GotoTab => GotoTab): goto_tab,

    /// Jump to a specific split.
    GotoSplit(GotoSplit => GotoSplit): goto_split,

    /// Jump to next/previous window.
    GotoWindow(GotoWindow => GotoWindow): goto_window,

    /// Resize the split in the given direction.
    ResizeSplit(ResizeSplit => ResizeSplit): resize_split,

    /// Equalize all the splits in the target window.
    EqualizeSplits: equalize_splits,

    /// Toggle whether a split is zoomed or not. A zoomed split is resized
    /// to take up the entire window.
    ToggleSplitZoom: toggle_split_zoom,

    /// Present the target terminal whether its a tab, split, or window.
    PresentTerminal: present_terminal,

    /// Sets a size limit (in pixels) for the target terminal.
    SizeLimit(SizeLimit => SizeLimit): size_limit,

    /// Resets the window size to the default size. See the
    /// `reset_window_size` keybinding for more information.
    ResetWindowSize: reset_window_size,

    /// Specifies the initial size of the target terminal.
    ///
    /// This may be sent once during the initialization of a surface
    /// (as part of the init call) to indicate the initial size requested
    /// for the window if it is not maximized or fullscreen.
    ///
    /// This may also be sent at any time after the surface is initialized
    /// to note the new "default size" of the window. This should in general
    /// be ignored, but may be useful if the apprt wants to support
    /// a "return to default size" action.
    InitialSize(InitialSize => InitialSize): initial_size,

    /// The cell size has changed to the given dimensions in pixels.
    CellSize(CellSize => CellSize): cell_size,

    /// The scrollbar is updating.
    Scrollbar(Scrollbar => ScrollbarC): scrollbar,

    /// The target should be re-rendered. This usually has a specific
    /// surface target but if the app is targeted then all active
    /// surfaces should be redrawn.
    Render: render,

    /// Control whether the inspector is shown or hidden.
    Inspector(Inspector => Inspector): inspector,

    /// Show the GTK inspector.
    ShowGtkInspector: show_gtk_inspector,

    /// The inspector for the given target has changes and should be
    /// rendered at the next opportunity.
    RenderInspector: render_inspector,

    /// Export the Terminal IO inspector event log.
    ExportTerminalIo(ExportTerminalIo<'a> => ExportTerminalIoC): export_terminal_io,

    /// Show a desktop notification.
    DesktopNotification(DesktopNotification<'a> => DesktopNotificationC): desktop_notification,

    /// Set the title of the target to the requested value.
    SetTitle(SetTitle<'a> => SetTitleC): set_title,

    /// Set the tab title override for the target's tab.
    SetTabTitle(SetTitle<'a> => SetTitleC): set_tab_title,

    /// Set the window title override for the target's tab.
    SetWindowTitle(SetTitle<'a> => SetTitleC): set_window_title,

    /// Set the title of the target to a prompted value. It is up to
    /// the apprt to prompt. The value specifies whether to prompt for the
    /// surface title or the tab title.
    PromptTitle(PromptTitle => PromptTitle): prompt_title,

    /// The current working directory has changed for the target terminal.
    Pwd(Pwd<'a> => PwdC): pwd,

    /// Set the mouse cursor shape.
    MouseShape(MouseShape => MouseShape): mouse_shape,

    /// Set whether the mouse cursor is visible or not.
    MouseVisibility(MouseVisibility => MouseVisibility): mouse_visibility,

    /// Called when the mouse is over or recently left a link.
    MouseOverLink(MouseOverLink<'a> => MouseOverLinkC): mouse_over_link,

    /// The health of the renderer has changed.
    RendererHealth(Health => Health): renderer_health,

    /// Open the Ghostty configuration. This is platform-specific about
    /// what it means; it can mean opening a dedicated UI or just opening
    /// a file in a text editor.
    OpenConfig(OpenConfig => OpenConfig): open_config,

    /// Called when there are no more surfaces and the app should quit
    /// after the configured delay.
    ///
    /// Despite the name, this is the notification that libghostty sends
    /// when there are no more surfaces regardless of if the configuration
    /// wants to quit after close, has any delay set, etc. It's up to the
    /// apprt to implement the proper logic based on the config.
    ///
    /// This can be cancelled by sending another quit_timer action with "stop".
    /// Multiple "starts" shouldn't happen and can be ignored or cause a
    /// restart it isn't that important.
    QuitTimer(QuitTimer => QuitTimer): quit_timer,

    /// Set the window floating state. A floating window is one that is
    /// always on top of other windows even when not focused.
    FloatWindow(FloatWindow => FloatWindow): float_window,

    /// Set the secure input functionality on or off. "Secure input" means
    /// that the user is currently at some sort of prompt where they may be
    /// entering a password or other sensitive information. This can be used
    /// by the app runtime to change the appearance of the cursor, setup
    /// system APIs to not log the input, etc.
    SecureInput(SecureInput => SecureInput): secure_input,

    /// A sequenced key binding has started, continued, or stopped.
    /// The UI should show some indication that the user is in a sequenced
    /// key mode because other input may be ignored.
    KeySequence(KeySequence => KeySequenceC): key_sequence,

    /// A key table has been activated or deactivated.
    KeyTable(KeyTable<'a> => KeyTableC): key_table,

    /// A terminal color was changed programmatically through things
    /// such as OSC 10/11.
    ColorChange(ColorChange => ColorChange): color_change,

    /// A request to reload the configuration. The reload request can be
    /// from a user or for some internal reason. The reload request may
    /// request it is a soft reload or a full reload. See the struct for
    /// more documentation.
    ///
    /// The configuration should be passed to updateConfig either at the
    /// app or surface level depending on the target.
    ReloadConfig(ReloadConfig => ReloadConfig): reload_config,

    /// The configuration has changed. The value is a pointer to the new
    /// configuration. The pointer is only valid for the duration of the
    /// action and should not be stored.
    ///
    /// This should be used by apprts to update any internal state that
    /// depends on configuration for the given target (i.e. headerbar colors).
    /// The apprt should copy any data it needs since the memory lifetime
    /// is only valid for the duration of the action.
    ///
    /// This allows an apprt to have config-dependent state reactively
    /// change without having to store the entire configuration or poll
    /// for changes.
    ConfigChange(ConfigChange<'a> => ConfigChangeC): config_change,

    /// Closes the currently focused window.
    CloseWindow: close_window,

    /// Called when the bell character is seen. The apprt should do whatever
    /// it needs to ring the bell. This is usually a sound or visual effect.
    RingBell: ring_bell,

    /// Called when the active selection changes. The apprt should read the
    /// current selection itself; this carries no payload.
    SelectionChanged: selection_changed,

    /// Undo the last action. See the "undo" keybinding for more
    /// details on what can and cannot be undone.
    Undo: undo,

    /// Redo the last undone action.
    Redo: redo,

    CheckForUpdates: check_for_updates,

    /// Open a URL using the native OS mechanisms. On macOS this might be `open`
    /// or on Linux this might be `xdg-open`. The exact mechanism is up to the
    /// apprt.
    OpenUrl(OpenUrl<'a> => OpenUrlC): open_url,

    /// Show a native GUI notification that the child process has exited.
    ShowChildExited(ChildExited => ChildExited): show_child_exited,

    /// Show a native GUI notification about the progress of some TUI operation.
    ProgressReport(ProgressReport => ProgressReportC): progress_report,

    /// Show the on-screen keyboard.
    ShowOnScreenKeyboard: show_on_screen_keyboard,

    /// A command has finished,
    CommandFinished(CommandFinished => CommandFinishedC): command_finished,

    /// Start the search overlay with an optional initial needle. If the
    /// search is already active and the needle is non-empty, update the
    /// current search needle and focus the search input.
    StartSearch(StartSearch<'a> => StartSearchC): start_search,

    /// End the search overlay, clearing the search state and hiding it.
    EndSearch: end_search,

    /// The total number of matches found by the search.
    SearchTotal(SearchTotal => SearchTotalC): search_total,

    /// The currently selected search match index (1-based).
    SearchSelected(SearchSelected => SearchSelectedC): search_selected,

    /// The readonly state of the surface has changed.
    Readonly(Readonly => Readonly): readonly,

    /// Copy the effective title of the surface to the clipboard.
    /// The effective title is the user-overridden title if set,
    /// otherwise the terminal-set title.
    CopyTitleToClipboard: copy_title_to_clipboard,

    /// Move a tab to a new window.
    MoveTabToNewWindow: move_tab_to_new_window,

    /// Resize the window containing the target surface so that the
    /// surface is the given size in points. A zero dimension should be
    /// kept as is. This is requested by the running program (CSI 8 t)
    /// and apprts may ignore it, for example if the surface is in a
    /// split or the window is fullscreen.
    ResizeWindow(ResizeWindow => ResizeWindow): resize_window,
}

/// Sync with: ghostty_action_s
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ActionC {
    pub key: ActionKey,
    pub value: ActionCValue,
}

// For ABI compatibility, we expect that this is our union size.
// At the time of writing, we don't promise ABI compatibility
// so we can change this but I want to be aware of it.
const _: () = assert!(
    std::mem::size_of::<ActionCValue>()
        == match std::mem::size_of::<usize>() {
            4 => 24,
            8 => 24,
            _ => unreachable!(),
        }
);

cval_identity!(
    CloseTabMode,
    SplitDirection,
    Fullscreen,
    MoveTab,
    GotoTab,
    GotoSplit,
    GotoWindow,
    ResizeSplit,
    SizeLimit,
    InitialSize,
    CellSize,
    Inspector,
    PromptTitle,
    MouseShape,
    MouseVisibility,
    Health,
    OpenConfig,
    QuitTimer,
    FloatWindow,
    SecureInput,
    ColorChange,
    ReloadConfig,
    ChildExited,
    Readonly,
    ResizeWindow,
);

impl CVal for Scrollbar {
    type C = ScrollbarC;
    fn cval(&self) -> ScrollbarC {
        Scrollbar::cval(*self)
    }
}

cval_forward!(
    ExportTerminalIo<'_> => ExportTerminalIoC,
    DesktopNotification<'_> => DesktopNotificationC,
    SetTitle<'_> => SetTitleC,
    Pwd<'_> => PwdC,
    MouseOverLink<'_> => MouseOverLinkC,
    KeySequence => KeySequenceC,
    KeyTable<'_> => KeyTableC,
    ConfigChange<'_> => ConfigChangeC,
    OpenUrl<'_> => OpenUrlC,
    ProgressReport => ProgressReportC,
    CommandFinished => CommandFinishedC,
    StartSearch<'_> => StartSearchC,
    SearchTotal => SearchTotalC,
    SearchSelected => SearchSelectedC,
);

crate::lib_enum! {
    // This is made extern (c_int) to make interop easier with our embedded
    // runtime. The small size cost doesn't make a difference in our union.
    pub enum SplitDirection: c {
        Right,
        Down,
        Left,
        Up,
    }
}

crate::lib_enum! {
    // This is made extern (c_int) to make interop easier with our embedded
    // runtime. The small size cost doesn't make a difference in our union.
    pub enum GotoSplit: c {
        Previous,
        Next,

        Up,
        Left,
        Down,
        Right,
    }
}

crate::lib_enum! {
    // This is made extern (c_int) to make interop easier with our embedded
    // runtime. The small size cost doesn't make a difference in our union.
    pub enum GotoWindow: c {
        Previous,
        Next,
    }
}

/// The amount to resize the split by and the direction to resize it in.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeSplit {
    pub amount: u16,
    pub direction: ResizeSplitDirection,
}

crate::lib_enum! {
    /// `ResizeSplit.Direction`
    pub enum ResizeSplitDirection: c {
        Up,
        Down,
        Left,
        Right,
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveTab {
    pub amount: isize,
}

/// The tab to jump to. This is non-exhaustive so that integer values represent
/// the index (zero-based) of the tab to jump to. Negative values are special
/// values.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GotoTab(pub c_int);

impl GotoTab {
    pub const PREVIOUS: GotoTab = GotoTab(-1);
    pub const NEXT: GotoTab = GotoTab(-2);
    pub const LAST: GotoTab = GotoTab(-3);

    // TODO: check non-exhaustive enums
    // test "ghostty.h GotoTab" {
    //     try lib.checkGhosttyHEnum(GotoTab, "GHOSTTY_GOTO_TAB_");
    // }
}

crate::lib_enum! {
    /// The fullscreen mode to toggle to if we're moving to fullscreen.
    pub enum Fullscreen: c {
        Native,

        /// macOS has a non-native fullscreen mode that is more like a maximized
        /// window. This is much faster to enter and exit than the native mode.
        MacosNonNative,
        MacosNonNativeVisibleMenu,
        MacosNonNativePaddedNotch,
    }
}

crate::lib_enum! {
    pub enum FloatWindow: c {
        On,
        Off,
        Toggle,
    }
}

crate::lib_enum! {
    pub enum SecureInput: c {
        On,
        Off,
        Toggle,
    }
}

crate::lib_enum! {
    /// The inspector mode to toggle to if we're toggling the inspector.
    pub enum Inspector: c {
        Toggle,
        Show,
        Hide,
    }
}

/// Terminal IO inspector contents to export. The contents are only valid for
/// the duration of the action callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportTerminalIo<'a> {
    pub contents: &'a [u8],
}

// Sync with: ghostty_action_export_terminal_io_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ExportTerminalIoC {
    pub contents: *const u8,
    pub len: usize,
}

impl ExportTerminalIo<'_> {
    pub fn cval(&self) -> ExportTerminalIoC {
        ExportTerminalIoC {
            contents: self.contents.as_ptr(),
            len: self.contents.len(),
        }
    }
}

impl fmt::Display for ExportTerminalIo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "apprt.action.ExportTerminalIO{{ contents: {} bytes }}",
            self.contents.len()
        )
    }
}

crate::lib_enum! {
    pub enum QuitTimer: c {
        Start,
        Stop,
    }
}

crate::lib_enum! {
    pub enum Readonly: c {
        Off,
        On,
    }
}

crate::lib_enum! {
    pub enum MouseVisibility: c {
        Visible,
        Hidden,
    }
}

crate::lib_enum! {
    /// Whether to prompt for the surface, tab, or window title.
    pub enum PromptTitle: c {
        Surface,
        Tab,
        Window,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseOverLink<'a> {
    pub url: &'a CStr,
}

// Sync with: ghostty_action_mouse_over_link_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MouseOverLinkC {
    pub url: *const u8,
    pub len: usize,
}

impl MouseOverLink<'_> {
    pub fn cval(&self) -> MouseOverLinkC {
        MouseOverLinkC {
            url: self.url.as_ptr() as *const u8,
            len: self.url.count_bytes(),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SizeLimit {
    pub min_width: u32,
    pub min_height: u32,
    pub max_width: u32,
    pub max_height: u32,
}

/// Note: Zig's `getGObjectType` (a GObject boxed type registration when
/// building the GTK runtime) is not ported here.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InitialSize {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResizeWindow {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetTitle<'a> {
    pub title: &'a CStr,
}

// Sync with: ghostty_action_set_title_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SetTitleC {
    pub title: *const c_char,
}

impl SetTitle<'_> {
    pub fn cval(&self) -> SetTitleC {
        SetTitleC {
            title: self.title.as_ptr(),
        }
    }
}

impl fmt::Display for SetTitle<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "apprt.action.SetTitle{{ {} }}",
            String::from_utf8_lossy(self.title.to_bytes())
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pwd<'a> {
    pub pwd: &'a CStr,
}

// Sync with: ghostty_action_set_pwd_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PwdC {
    pub pwd: *const c_char,
}

impl Pwd<'_> {
    pub fn cval(&self) -> PwdC {
        PwdC {
            pwd: self.pwd.as_ptr(),
        }
    }
}

impl fmt::Display for Pwd<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "apprt.action.Pwd{{ {} }}",
            String::from_utf8_lossy(self.pwd.to_bytes())
        )
    }
}

/// The desktop notification to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopNotification<'a> {
    pub title: &'a CStr,
    pub body: &'a CStr,
}

// Sync with: ghostty_action_desktop_notification_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DesktopNotificationC {
    pub title: *const c_char,
    pub body: *const c_char,
}

impl DesktopNotification<'_> {
    pub fn cval(&self) -> DesktopNotificationC {
        DesktopNotificationC {
            title: self.title.as_ptr(),
            body: self.body.as_ptr(),
        }
    }
}

impl fmt::Display for DesktopNotification<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "apprt.action.DesktopNotification{{ title: {}, body: {} }}",
            String::from_utf8_lossy(self.title.to_bytes()),
            String::from_utf8_lossy(self.body.to_bytes()),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySequence {
    Trigger(Trigger),
    End,
}

// Sync with: ghostty_action_key_sequence_s
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KeySequenceC {
    pub active: bool,
    pub trigger: TriggerC,
}

impl KeySequence {
    pub fn cval(&self) -> KeySequenceC {
        match self {
            KeySequence::Trigger(t) => KeySequenceC {
                active: true,
                trigger: t.cval(),
            },
            KeySequence::End => KeySequenceC {
                active: false,
                trigger: TriggerC::default(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTable<'a> {
    Activate(&'a [u8]),
    Deactivate,
    DeactivateAll,
}

crate::lib_enum! {
    // Sync with: ghostty_action_key_table_tag_e
    pub enum KeyTableTag: c {
        Activate,
        Deactivate,
        DeactivateAll,
    }
}

/// `KeyTable.CValue.activate`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KeyTableActivateC {
    pub name: *const u8,
    pub len: usize,
}

// Sync with: ghostty_action_key_table_u
#[repr(C)]
#[derive(Clone, Copy)]
pub union KeyTableCValue {
    pub activate: KeyTableActivateC,
}

// Sync with: ghostty_action_key_table_s
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KeyTableC {
    pub tag: KeyTableTag,
    pub value: KeyTableCValue,
}

impl KeyTable<'_> {
    pub fn cval(&self) -> KeyTableC {
        match self {
            KeyTable::Activate(name) => KeyTableC {
                tag: KeyTableTag::Activate,
                value: KeyTableCValue {
                    activate: KeyTableActivateC {
                        name: name.as_ptr(),
                        len: name.len(),
                    },
                },
            },
            // Zig leaves the value undefined; zero it for determinism.
            KeyTable::Deactivate => KeyTableC {
                tag: KeyTableTag::Deactivate,
                value: KeyTableCValue {
                    activate: KeyTableActivateC {
                        name: std::ptr::null(),
                        len: 0,
                    },
                },
            },
            KeyTable::DeactivateAll => KeyTableC {
                tag: KeyTableTag::DeactivateAll,
                value: KeyTableCValue {
                    activate: KeyTableActivateC {
                        name: std::ptr::null(),
                        len: 0,
                    },
                },
            },
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorChange {
    pub kind: ColorKind,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Negative numbers indicate some named kind; 0+ values indicate a palette
/// index. (Non-exhaustive `enum(c_int)` in Zig.)
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorKind(pub c_int);

impl ColorKind {
    // Negative numbers indicate some named kind
    pub const FOREGROUND: ColorKind = ColorKind(-1);
    pub const BACKGROUND: ColorKind = ColorKind(-2);
    pub const CURSOR: ColorKind = ColorKind(-3);

    // TODO: check non-non-exhaustive enums
    // test "ghostty.h ColorKind" {
    //     try lib.checkGhosttyHEnum(ColorKind, "GHOSTTY_COLOR_KIND_");
    // }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReloadConfig {
    /// A soft reload means that the configuration doesn't need to be
    /// read off disk, but libghostty needs the full config again so call
    /// updateConfig with it.
    pub soft: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ConfigChange<'a> {
    pub config: &'a Config,
}

// Sync with: ghostty_action_config_change_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ConfigChangeC {
    pub config: *const Config,
}

impl ConfigChange<'_> {
    pub fn cval(&self) -> ConfigChangeC {
        ConfigChangeC {
            config: self.config,
        }
    }
}

/// Open a URL or file path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenUrl<'a> {
    /// Where the URL came from and what it likely points to.
    pub kind: OpenUrlKind,

    /// The URL to open. Depending on `kind`, this may be a plain file
    /// path or arbitrary text rather than a well-formed URL.
    pub url: &'a [u8],
}

crate::lib_enum! {
    /// Where the URL came from, used to decide how to open it.
    ///
    /// `text` and `html` come from Ghostty itself and can be opened
    /// directly. `unknown` and `osc8` may come from terminal output, which
    /// any program can control, so apprts must check them before opening.
    ///
    /// Sync with: ghostty_action_open_url_kind_e
    pub enum OpenUrlKind: c {
        /// Untrusted. The source is unknown, such as text that a `link`
        /// config entry matched in terminal output. It may be a plain path
        /// instead of a URL, for example:
        ///
        /// ```text
        /// https://example.com
        /// ~/notes.txt
        /// src/main.zig
        /// ```
        Unknown,

        /// The URL is known to be a text file. Open it in a text editor
        /// or viewer if possible.
        Text,

        /// The URL is known to contain HTML content.
        Html,

        /// Untrusted. The target of an OSC 8 hyperlink, exactly as the
        /// program sent it. The visible link text may not match it.
        Osc8,
    }
}

// Sync with: ghostty_action_open_url_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OpenUrlC {
    pub kind: OpenUrlKind,
    pub url: *const u8,
    pub len: usize,
}

impl OpenUrl<'_> {
    pub fn cval(&self) -> OpenUrlC {
        OpenUrlC {
            kind: self.kind,
            url: self.url.as_ptr(),
            len: self.url.len(),
        }
    }
}

crate::lib_enum! {
    /// sync with ghostty_action_close_tab_mode_e in ghostty.h
    pub enum CloseTabMode: c {
        /// Close the current tab.
        This,
        /// Close all other tabs.
        Other,
        /// Close all tabs to the right of the current tab.
        Right,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandFinished {
    pub exit_code: Option<u8>,
    pub duration: ConfigDuration,
}

/// sync with ghostty_action_command_finished_s in ghostty.h
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandFinishedC {
    pub exit_code: i16,
    pub duration: u64,
}

impl CommandFinished {
    pub fn cval(&self) -> CommandFinishedC {
        CommandFinishedC {
            exit_code: match self.exit_code {
                Some(v) => v as i16,
                None => -1,
            },
            duration: self.duration.duration,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartSearch<'a> {
    pub needle: &'a CStr,
}

// Sync with: ghostty_action_start_search_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StartSearchC {
    pub needle: *const c_char,
}

impl StartSearch<'_> {
    pub fn cval(&self) -> StartSearchC {
        StartSearchC {
            needle: self.needle.as_ptr(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchTotal {
    pub total: Option<usize>,
}

// Sync with: ghostty_action_search_total_s
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchTotalC {
    pub total: isize,
}

impl SearchTotal {
    pub fn cval(&self) -> SearchTotalC {
        SearchTotalC {
            total: match self.total {
                Some(t) => isize::try_from(t).expect("search total overflows isize"),
                None => -1,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchSelected {
    pub selected: Option<usize>,
}

// Sync with: ghostty_action_search_selected_s
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchSelectedC {
    pub selected: isize,
}

impl SearchSelected {
    pub fn cval(&self) -> SearchSelectedC {
        SearchSelectedC {
            selected: match self.selected {
                Some(s) => isize::try_from(s).expect("search selected overflows isize"),
                None => -1,
            },
        }
    }
}

crate::lib_enum! {
    /// sync with ghostty_action_close_tab_mode_e in ghostty.h
    pub enum OpenConfig: c {
        /// Open the config in the OS default editor.
        OsOpen,

        /// Open the config in a new window using $EDITOR or $VISUAL
        NewWindow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::check_ghostty_h_enum;
    use crate::lib::r#enum::LibEnum;

    const GHOSTTY_H: &str = include_str!("../../include/ghostty.h");

    fn check<T: LibEnum>(prefix: &str) {
        check_ghostty_h_enum::<T>(prefix, GHOSTTY_H).unwrap();
    }

    #[test]
    fn ghostty_h_target_key() {
        check::<TargetKey>("GHOSTTY_TARGET_");
    }

    #[test]
    fn ghostty_h_action_key() {
        check::<ActionKey>("GHOSTTY_ACTION_");
    }

    #[test]
    fn ghostty_h_split_direction() {
        check::<SplitDirection>("GHOSTTY_SPLIT_DIRECTION_");
    }

    #[test]
    fn ghostty_h_goto_split() {
        check::<GotoSplit>("GHOSTTY_GOTO_SPLIT_");
    }

    #[test]
    fn ghostty_h_goto_window() {
        check::<GotoWindow>("GHOSTTY_GOTO_WINDOW_");
    }

    #[test]
    fn ghostty_h_resize_split_direction() {
        check::<ResizeSplitDirection>("GHOSTTY_RESIZE_SPLIT_");
    }

    #[test]
    fn ghostty_h_fullscreen() {
        check::<Fullscreen>("GHOSTTY_FULLSCREEN_");
    }

    #[test]
    fn ghostty_h_float_window() {
        check::<FloatWindow>("GHOSTTY_FLOAT_WINDOW_");
    }

    #[test]
    fn ghostty_h_secure_input() {
        check::<SecureInput>("GHOSTTY_SECURE_INPUT_");
    }

    #[test]
    fn ghostty_h_inspector() {
        check::<Inspector>("GHOSTTY_INSPECTOR_");
    }

    #[test]
    fn ghostty_h_quit_timer() {
        check::<QuitTimer>("GHOSTTY_QUIT_TIMER_");
    }

    #[test]
    fn ghostty_h_readonly() {
        check::<Readonly>("GHOSTTY_READONLY_");
    }

    #[test]
    fn ghostty_h_mouse_visibility() {
        check::<MouseVisibility>("GHOSTTY_MOUSE_");
    }

    #[test]
    fn ghostty_h_prompt_title() {
        check::<PromptTitle>("GHOSTTY_PROMPT_TITLE_");
    }

    #[test]
    fn ghostty_h_open_url_kind() {
        check::<OpenUrlKind>("GHOSTTY_ACTION_OPEN_URL_KIND_");
    }

    #[test]
    fn ghostty_h_close_tab_mode() {
        check::<CloseTabMode>("GHOSTTY_ACTION_CLOSE_TAB_MODE_");
    }

    #[test]
    fn ghostty_h_open_config() {
        check::<OpenConfig>("GHOSTTY_ACTION_OPEN_CONFIG_");
    }

    // Extra (not in Zig): the non-exhaustive enums and the key table tag
    // also match the header.
    #[test]
    fn ghostty_h_non_exhaustive_and_key_table() {
        check::<KeyTableTag>("GHOSTTY_KEY_TABLE_");
        let c = crate::lib::r#enum::parse_c_constants(GHOSTTY_H);
        assert_eq!(Some(&-1), c.get("GHOSTTY_GOTO_TAB_PREVIOUS"));
        assert_eq!(Some(&(GotoTab::NEXT.0 as i64)), c.get("GHOSTTY_GOTO_TAB_NEXT"));
        assert_eq!(Some(&(GotoTab::LAST.0 as i64)), c.get("GHOSTTY_GOTO_TAB_LAST"));
    }

    // Zig: comptime assertion on @sizeOf(CValue).
    #[test]
    fn c_value_size() {
        assert_eq!(24, std::mem::size_of::<ActionCValue>());
        assert_eq!(
            std::mem::size_of::<c_int>() + 4 + 24,
            std::mem::size_of::<ActionC>()
        );
        assert_eq!(std::mem::size_of::<usize>(), std::mem::align_of::<ActionCValue>());
    }

    #[test]
    fn c_layouts() {
        assert_eq!(16, std::mem::size_of::<KeySequenceC>());
        assert_eq!(24, std::mem::size_of::<OpenUrlC>());
        assert_eq!(24, std::mem::size_of::<ScrollbarC>());
        assert_eq!(16, std::mem::size_of::<CommandFinishedC>());
        assert_eq!(16, std::mem::size_of::<ChildExited>());
        assert_eq!(8, std::mem::size_of::<ColorChange>());
        assert_eq!(8, std::mem::size_of::<ResizeSplit>());
    }

    #[test]
    fn action_cval() {
        let title = c"hello";
        let a = Action::SetTitle(SetTitle { title });
        assert_eq!(ActionKey::SetTitle, a.key());
        let c = a.cval();
        assert_eq!(ActionKey::SetTitle, c.key);
        // SAFETY: the key says set_title is active.
        assert_eq!(title.as_ptr(), unsafe { c.value.set_title.title });

        let c = Action::SearchTotal(SearchTotal { total: None }).cval();
        assert_eq!(-1, unsafe { c.value.search_total.total });

        let c = Action::CommandFinished(CommandFinished {
            exit_code: Some(3),
            duration: ConfigDuration { duration: 42 },
        })
        .cval();
        let v = unsafe { c.value.command_finished };
        assert_eq!(3, v.exit_code);
        assert_eq!(42, v.duration);

        let c = Action::GotoTab(GotoTab::LAST).cval();
        assert_eq!(GotoTab::LAST, unsafe { c.value.goto_tab });

        assert_eq!(ActionKey::Quit, Action::Quit.cval().key);
        assert_eq!(0, ActionKey::Quit.backing());
        assert_eq!(70, ActionKey::KEYS_LEN);
    }

    #[test]
    fn key_sequence_and_table_cval() {
        let c = KeySequence::End.cval();
        assert!(!c.active);
        let c = KeySequence::Trigger(Trigger::default()).cval();
        assert!(c.active);

        let c = KeyTable::Activate(b"vim").cval();
        assert_eq!(KeyTableTag::Activate, c.tag);
        assert_eq!(3, unsafe { c.value.activate.len });
        assert_eq!(KeyTableTag::DeactivateAll, KeyTable::DeactivateAll.cval().tag);
    }

    #[test]
    fn target_cval() {
        let mut rt = apprt::Surface::default();
        // Only the `rt_surface` field is read, so the rest of the (large)
        // core surface can stay uninitialized.
        let mut core = std::mem::MaybeUninit::<CoreSurface>::uninit();
        // SAFETY: writing a field of the uninitialized struct in place.
        unsafe { std::ptr::addr_of_mut!((*core.as_mut_ptr()).rt_surface).write(&mut rt) };
        let t = Target::Surface(core.as_mut_ptr());
        let c = t.cval();
        assert_eq!(TargetKey::Surface, c.key);
        assert_eq!(&mut rt as *mut apprt::Surface, unsafe { c.value.surface });
        assert_eq!(TargetKey::App, Target::App.cval().key);
    }

    #[test]
    fn format() {
        assert_eq!(
            "apprt.action.SetTitle{ hi }",
            SetTitle { title: c"hi" }.to_string()
        );
        assert_eq!(
            "apprt.action.ExportTerminalIO{ contents: 3 bytes }",
            ExportTerminalIo { contents: b"abc" }.to_string()
        );
    }
}
