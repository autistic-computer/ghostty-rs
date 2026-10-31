//! Port of `src/apprt/gtk/ipc/new_window.zig`.
//!
//! STATUS: ported

use glib::prelude::*;

use super::d_bus::{arguments_strings, DBus, Errors, NewWindow, Target};

// Use a D-Bus method call to open a new window on GTK.
// See: https://wiki.gnome.org/Projects/GLib/GApplication/DBusAPI
//
// `ghostty +new-window` is equivalent to the following command (on a release build):
//
// ```
// gdbus call --session --dest com.mitchellh.ghostty --object-path /com/mitchellh/ghostty --method org.gtk.Actions.Activate new-window [] []
// ```
//
// `ghostty +new-window -e echo hello` would be equivalent to the following command (on a release build):
//
// ```
// gdbus call --session --dest com.mitchellh.ghostty --object-path /com/mitchellh/ghostty --method org.gtk.Actions.Activate new-window-command '[<@as ["-e" "echo" "hello"]>]' []
// ```
pub fn new_window(target: Target, value: &NewWindow) -> Result<bool, Errors> {
    let mut dbus = DBus::init(target, action_name(value))?;

    if let Some(arguments) = value.arguments {
        // If any arguments were specified on the command line, the first
        // parameter is an array of strings that contain the arguments. They
        // will be sent to the main Ghostty instance and interpreted as CLI
        // arguments.
        dbus.add_parameter(arguments_strings(arguments).to_variant());
    }

    dbus.send()?;

    Ok(true)
}

fn action_name(value: &NewWindow) -> &'static str {
    if value.arguments.is_none() {
        "new-window"
    } else {
        "new-window-command"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_depends_on_arguments() {
        assert_eq!(action_name(&NewWindow { arguments: None }), "new-window");
        assert_eq!(
            action_name(&NewWindow {
                arguments: Some(&[c"-e"])
            }),
            "new-window-command"
        );
        assert_eq!(
            vec!["-e".to_owned(), "echo".to_owned()].to_variant().print(true).as_str(),
            "['-e', 'echo']"
        );
    }
}
