//! Port of `src/apprt/gtk/ipc/toggle_quick_terminal.zig`.
//!
//! STATUS: ported

use super::d_bus::{DBus, Errors, Target};

/// Use a D-Bus method call to toggle the quick terminal on GTK.
///
/// `ghostty +toggle-quick-terminal` is equivalent to the following command
/// (on a release build):
///
/// ```sh
/// gdbus call --session \
///   --dest com.mitchellh.ghostty \
///   --object-path /com/mitchellh/ghostty \
///   --method org.gtk.Actions.Activate \
///   toggle-quick-terminal [] []
/// ```
pub fn toggle_quick_terminal(target: Target) -> Result<bool, Errors> {
    let mut dbus = DBus::init(target, "toggle-quick-terminal")?;
    dbus.send()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_target_fails() {
        assert_eq!(
            toggle_quick_terminal(Target::Class(c"bad name")),
            Err(Errors::IpcFailed)
        );
    }
}
