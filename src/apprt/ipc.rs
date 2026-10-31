//! Port of `src/apprt/ipc.zig`.
//!
//! STATUS: ported
//!
//! Inter-process Communication to a running Ghostty instance from a separate
//! process.
//!
//! Deviations:
//! * Zig's `Action.Value(key)` type function has no counterpart; each
//!   [`Action`] variant carries its payload directly.
//! * Zig's `NewTab.cval`/`NewTab.C.deinit` take/return `NewWindow` types
//!   (a latent bug that never compiles because `Action.cval` is never
//!   instantiated); the Rust port uses the `NewTab` types and keeps
//!   `surface_id`.
//! * The C argument arrays are allocated with the global allocator; free
//!   them with the `deinit` methods on the C structs.

use std::ffi::{c_char, CStr};
use std::fmt;
use std::ptr;


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Errors {
    /// The IPC failed. If a function returns this error, it's expected that
    /// an a more specific error message will have been written to stderr (or
    /// otherwise shown to the user in an appropriate way).
    IpcFailed,
}

impl fmt::Display for Errors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Errors::IpcFailed => f.write_str("IPCFailed"),
        }
    }
}

impl std::error::Error for Errors {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// Open up a new window in a custom instance of Ghostty.
    Class(&'a CStr),

    /// Detect which instance to open a new window in.
    Detect,
}

crate::lib_enum! {
    // Sync with: ghostty_ipc_target_tag_e
    pub enum TargetKey: c {
        Class,
        Detect,
    }
}

/// Sync with: ghostty_ipc_target_u
#[repr(C)]
#[derive(Clone, Copy)]
pub union TargetCValue {
    pub class: *const c_char,
    pub detect: (),
}

/// Sync with: ghostty_ipc_target_s
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TargetC {
    pub key: TargetKey,
    pub value: TargetCValue,
}

impl Target<'_> {
    /// The active tag.
    pub fn key(&self) -> TargetKey {
        match self {
            Target::Class(_) => TargetKey::Class,
            Target::Detect => TargetKey::Detect,
        }
    }

    /// Convert to ghostty_ipc_target_s.
    pub fn cval(&self) -> TargetC {
        TargetC {
            key: self.key(),
            value: match self {
                Target::Class(class) => TargetCValue {
                    class: class.as_ptr(),
                },
                Target::Detect => TargetCValue { detect: () },
            },
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action<'a> {
    /// Create a new window.
    NewWindow(NewWindow<'a>),

    /// Create a new tab.
    NewTab(NewTab<'a>),

    /// Toggle the quick terminal.
    ToggleQuickTerminal,
}

/// Build a null-terminated C array of argument pointers.
fn c_arguments(arguments: Option<&[&CStr]>) -> *mut *const c_char {
    let Some(arguments) = arguments else {
        return ptr::null_mut();
    };

    let mut result: Vec<*const c_char> = Vec::with_capacity(arguments.len() + 1);
    for argument in arguments {
        result.push(argument.as_ptr());
    }

    // add null terminator
    result.push(ptr::null());

    Box::into_raw(result.into_boxed_slice()) as *mut *const c_char
}

/// Free an array built by [`c_arguments`].
///
/// # Safety
///
/// `arguments` must be null or a pointer returned by [`c_arguments`] that
/// has not been freed yet.
unsafe fn free_c_arguments(arguments: *mut *const c_char) {
    if arguments.is_null() {
        return;
    }

    // Find the null terminator to recover the allocation length.
    let mut len = 0;
    // SAFETY: the array is null terminated (see c_arguments).
    while !unsafe { *arguments.add(len) }.is_null() {
        len += 1;
    }

    // SAFETY: allocated by c_arguments as a boxed slice of len + 1.
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(arguments, len + 1)) });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NewWindow<'a> {
    /// A list of configuration entries that will override the configuration
    /// of the first surface of the new window. If this is `null` there
    /// are no overrides. Note that not all configuration entries may be
    /// overridden.
    ///
    /// It is an error for this to be non-`null`, but zero length.
    pub arguments: Option<&'a [&'a CStr]>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NewWindowC {
    /// A list of configuration entries that will override the
    /// configuration of the first surface of the new tab. If this is
    /// `null` there are no overrides. Note that not all configuration
    /// entries may be overridden.
    ///
    /// It is an error for this to be non-`null`, but zero length.
    pub arguments: *mut *const c_char,
}

impl NewWindowC {
    pub fn deinit(&mut self) {
        // SAFETY: arguments is null or was built by NewWindow::cval.
        unsafe { free_c_arguments(self.arguments) };
        self.arguments = ptr::null_mut();
    }
}

impl NewWindow<'_> {
    pub fn cval(&self) -> NewWindowC {
        NewWindowC {
            arguments: c_arguments(self.arguments),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NewTab<'a> {
    /// The unique ID of a surface. If the ID is zero the currently focused
    /// surface is assumed. This will be used to identify the window to add
    /// the tab to.
    pub surface_id: u64,

    /// A list of configuration entries that will override the configuration
    /// of the first surface of the new tab. If this is `null` there
    /// are no overrides. Note that not all configuration entries may be
    /// overridden.
    ///
    /// It is an error for this to be non-`null`, but zero length.
    pub arguments: Option<&'a [&'a CStr]>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NewTabC {
    /// The unique ID of a surface. If the ID is zero the currently
    /// focused surface is assumed. This will be used to identify the
    /// window to add the tab to.
    pub surface_id: u64,
    /// A list of configuration entries that will override the
    /// configuration of the first surface of the new tab. If this is
    /// `null` there are no overrides. Note that not all configuration
    /// entries may be overridden.
    ///
    /// It is an error for this to be non-`null`, but zero length.
    pub arguments: *mut *const c_char,
}

impl NewTabC {
    pub fn deinit(&mut self) {
        // SAFETY: arguments is null or was built by NewTab::cval.
        unsafe { free_c_arguments(self.arguments) };
        self.arguments = ptr::null_mut();
    }
}

impl NewTab<'_> {
    pub fn cval(&self) -> NewTabC {
        NewTabC {
            surface_id: self.surface_id,
            arguments: c_arguments(self.arguments),
        }
    }
}

crate::lib_enum! {
    /// Sync with: ghostty_ipc_action_tag_e
    pub enum ActionKey: c {
        NewWindow,
        NewTab,
        ToggleQuickTerminal,
    }
}

/// Sync with: ghostty_ipc_action_u
#[repr(C)]
#[derive(Clone, Copy)]
pub union ActionCValue {
    pub new_window: NewWindowC,
    pub new_tab: NewTabC,
    pub toggle_quick_terminal: (),
}

/// Sync with: ghostty_ipc_action_s
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
            4 => 12,
            8 => 16,
            _ => unreachable!(),
        }
);

impl Action<'_> {
    /// The active tag.
    pub fn key(&self) -> ActionKey {
        match self {
            Action::NewWindow(_) => ActionKey::NewWindow,
            Action::NewTab(_) => ActionKey::NewTab,
            Action::ToggleQuickTerminal => ActionKey::ToggleQuickTerminal,
        }
    }

    /// Convert to ghostty_ipc_action_s. Any allocated argument arrays must
    /// be freed with the `deinit` method of the active C value.
    pub fn cval(&self) -> ActionC {
        let value = match self {
            Action::NewWindow(v) => ActionCValue {
                new_window: v.cval(),
            },
            Action::NewTab(v) => ActionCValue { new_tab: v.cval() },
            Action::ToggleQuickTerminal => ActionCValue {
                toggle_quick_terminal: (),
            },
        };

        ActionC {
            key: self.key(),
            value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::check_ghostty_h_enum;

    const GHOSTTY_H: &str = include_str!("../../include/ghostty.h");

    #[test]
    fn ghostty_h_target_key() {
        check_ghostty_h_enum::<TargetKey>("GHOSTTY_IPC_TARGET_", GHOSTTY_H).unwrap();
    }

    #[test]
    fn ghostty_h_action_key() {
        check_ghostty_h_enum::<ActionKey>("GHOSTTY_IPC_ACTION_", GHOSTTY_H).unwrap();
    }

    #[test]
    fn new_window_cval() {
        let a = c"--title=a";
        let b = c"--title=b";
        let args = [a, b];
        let action = Action::NewWindow(NewWindow {
            arguments: Some(&args),
        });
        let mut c = action.cval();
        assert_eq!(ActionKey::NewWindow, c.key);
        // SAFETY: the key says new_window is active.
        let nw = unsafe { &mut c.value.new_window };
        unsafe {
            assert_eq!(a.as_ptr(), *nw.arguments);
            assert_eq!(b.as_ptr(), *nw.arguments.add(1));
            assert!((*nw.arguments.add(2)).is_null());
        }
        nw.deinit();
        assert!(nw.arguments.is_null());

        let none = NewWindow { arguments: None }.cval();
        assert!(none.arguments.is_null());
    }

    #[test]
    fn new_tab_cval() {
        let args = [c"--x=1"];
        let mut c = NewTab {
            surface_id: 42,
            arguments: Some(&args),
        }
        .cval();
        assert_eq!(42, c.surface_id);
        assert!(!c.arguments.is_null());
        c.deinit();
    }

    #[test]
    fn target_cval() {
        let c = Target::Class(c"com.example").cval();
        assert_eq!(TargetKey::Class, c.key);
        let s = unsafe { CStr::from_ptr(c.value.class) };
        assert_eq!(c"com.example", s);
        assert_eq!(TargetKey::Detect, Target::Detect.cval().key);
    }
}
