//! Port of `src/apprt/gtk/ipc/DBus.zig`.
//!
//! STATUS: ported
//!
//! DBus helper for IPC
//!
//! gtk-rs has no `GVariantBuilder` wrapper, so the payload and parameter
//! builders are kept as the values collected so far and turned into the
//! `(sava{sv})` payload in [`DBus::send`].

use crate::apprt::gtk::build::info as build_info;

pub use crate::apprt::ipc::{Errors, NewTab, NewWindow, Target};

/// The arguments of an IPC action as D-Bus strings (`[:0]const u8` sent
/// with `glib.Variant.newFromBytes`; D-Bus strings must be UTF-8).
pub(crate) fn arguments_strings(arguments: &[&std::ffi::CStr]) -> Vec<String> {
    arguments
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

pub struct DBus<'a> {
    /// The target for this IPC.
    pub target: Target<'a>,

    /// Connection to the DBus session bus.
    pub dbus: gio::DBusConnection,

    /// The bus name of the Ghostty instance that we are calling.
    pub bus_name: String,

    /// The object path of the Ghostty instance that we are calling.
    pub object_path: String,

    /// Used to build the DBus payload (the action name).
    action: String,

    /// Used to build the parameters for the IPC.
    parameters: Vec<glib::Variant>,
}

pub type InitError = Errors;

/// The bus name and object path for contacting the Ghostty instance we're
/// interested in.
fn bus_name_and_object_path(target: &Target) -> (String, String) {
    match target {
        Target::Class(class) => {
            let class = class.to_string_lossy().into_owned();
            // Force the usage of the class specified on the CLI to determine the
            // bus name and object path.
            let object_path = format!("/{class}").replace('.', "/").replace('-', "_");
            (class, object_path)
        }
        Target::Detect => (
            build_info::APPLICATION_ID.to_owned(),
            build_info::OBJECT_PATH.to_owned(),
        ),
    }
}

/// Checks the bus name and object path, reporting problems on stderr.
fn validate(bus_name: &str, object_path: &str) -> Result<(), Errors> {
    if !gio::Application::id_is_valid(bus_name) {
        eprintln!("D-Bus bus name is not valid: {bus_name}");
        return Err(Errors::IpcFailed);
    }

    if !glib::Variant::is_object_path(object_path) {
        eprintln!("D-Bus object path is not valid: {object_path}");
        return Err(Errors::IpcFailed);
    }
    Ok(())
}

impl<'a> DBus<'a> {
    /// Initialize the helper.
    pub fn init(target: Target<'a>, action: &str) -> Result<DBus<'a>, InitError> {
        // Get the appropriate bus name and object path for contacting the
        // Ghostty instance we're interested in.
        let (bus_name, object_path) = bus_name_and_object_path(&target);

        validate(&bus_name, &object_path)?;

        // Get a connection to the DBus session bus.
        let dbus = match gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) {
            Ok(dbus) => dbus,
            Err(err) => {
                eprintln!(
                    "Unable to establish connection to D-Bus session bus: {}",
                    err.message()
                );
                return Err(Errors::IpcFailed);
            }
        };

        Ok(DBus {
            target,
            dbus,
            bus_name,
            object_path,
            action: action.to_owned(),
            parameters: Vec::new(),
        })
    }

    /// Add a parameter to the IPC call.
    pub fn add_parameter(&mut self, variant: glib::Variant) {
        self.parameters.push(glib::Variant::from_variant(&variant));
    }

    /// The `(sava{sv})` payload of `org.gtk.Actions.Activate`.
    fn payload(&self) -> glib::Variant {
        payload(&self.action, &self.parameters)
    }

    /// Send the IPC to the remote Ghostty. Once it completes, nothing further
    /// should be done with this object other than call `deinit`.
    pub fn send(&mut self) -> Result<(), Errors> {
        let payload = self.payload();

        let result = self.dbus.call_sync(
            Some(&self.bus_name),
            &self.object_path,
            "org.gtk.Actions",
            "Activate",
            Some(&payload),
            None, // We don't care about the return type, we don't do anything with it.
            gio::DBusCallFlags::NONE, // no flags
            -1,                       // default timeout
            None::<&gio::Cancellable>, // not cancellable
        );

        if let Err(err) = result {
            eprintln!("D-Bus method call returned an error err={}", err.message());
            return Err(Errors::IpcFailed);
        }
        Ok(())
    }

    /// Free/unref any data held by this instance.
    pub fn deinit(self) {}
}

fn payload(action: &str, parameters: &[glib::Variant]) -> glib::Variant {
    use glib::prelude::*;

    // finish building the parameters
    let parameters =
        glib::Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, parameters.iter().cloned());

    // Add the platform data to the payload. We have no platform data.
    let platform_data = glib::Variant::array_from_iter_with_type(
        glib::VariantTy::new("{sv}").unwrap(),
        std::iter::empty::<glib::Variant>(),
    );

    glib::Variant::tuple_from_iter([action.to_variant(), parameters, platform_data])
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::prelude::*;

    #[test]
    fn class_target_paths() {
        let (bus, path) = bus_name_and_object_path(&Target::Class(c"com.example.my-app"));
        assert_eq!(bus, "com.example.my-app");
        assert_eq!(path, "/com/example/my_app");
        assert!(validate(&bus, &path).is_ok());

        let (bus, path) = bus_name_and_object_path(&Target::Detect);
        assert_eq!(bus, build_info::APPLICATION_ID);
        assert_eq!(path, build_info::OBJECT_PATH);
        assert!(validate(&bus, &path).is_ok());
    }

    #[test]
    fn invalid_class_fails_before_connecting() {
        assert_eq!(DBus::init(Target::Class(c"not a bus name"), "new-window").err(), Some(Errors::IpcFailed));
    }

    #[test]
    fn payload_shape() {
        let p = payload("new-tab", &[glib::Variant::from_variant(&(0u64, Vec::<String>::new()).to_variant())]);
        assert_eq!(p.type_().as_str(), "(sava{sv})");
        assert_eq!(p.print(true).as_str(), "('new-tab', [<(uint64 0, @as [])>], @a{sv} {})");
    }
}
