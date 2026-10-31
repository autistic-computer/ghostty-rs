//! Port of `src/apprt/gtk/ipc/new_tab.zig`.
//!
//! STATUS: ported

use glib::prelude::*;

use super::d_bus::{arguments_strings, DBus, Errors, NewTab, Target};

// Use a D-Bus method call to open a new tab on GTK.
// See: https://wiki.gnome.org/Projects/GLib/GApplication/DBusAPI
//
// `ghostty +new-tab` is equivalent to the following command (on a release build):
//
// ```
// gdbus call --session --dest com.mitchellh.ghostty --object-path /com/mitchellh/ghostty --method org.gtk.Actions.Activate new-tab '[<@(tas) (0, [])>]' []
// ```
//
// `ghostty +new-tab -e echo hello` would be equivalent to the following command (on a release build):
//
// ```
// gdbus call --session --dest com.mitchellh.ghostty --object-path /com/mitchellh/ghostty --method org.gtk.Actions.Activate new-tab '[<@(tas) (0, ["-e" "echo" "hello"])>]' []
// ```
pub fn new_tab(target: Target, value: &NewTab) -> Result<bool, Errors> {
    let mut dbus = DBus::init(target, "new-tab")?;

    dbus.add_parameter(parameter(value));

    dbus.send()?;

    Ok(true)
}

/// The `(tas)` parameter: the target surface ID, and the command line
/// arguments (if any) that will be sent to the main Ghostty instance and
/// interpreted as CLI arguments.
fn parameter(value: &NewTab) -> glib::Variant {
    let command: Vec<String> = value.arguments.map(arguments_strings).unwrap_or_default();
    (value.surface_id, command).to_variant()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_shape() {
        let p = parameter(&NewTab {
            surface_id: 0,
            arguments: Some(&[c"-e", c"echo", c"hello"]),
        });
        assert_eq!(p.print(true).as_str(), "(uint64 0, ['-e', 'echo', 'hello'])");
        assert_eq!(parameter(&NewTab::default()).print(true).as_str(), "(uint64 0, @as [])");
    }
}
