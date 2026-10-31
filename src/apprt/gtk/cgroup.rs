//! Port of `src/apprt/gtk/cgroup.zig`.
//!
//! STATUS: ported
//!
//! Contains all the logic for putting individual surfaces into
//! transient systemd scopes.
#![cfg(feature = "gtk")]

use gio::prelude::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub memory_high: Option<u64>,
    pub tasks_max: Option<u64>,
}

/// `error{DbusCallFailed}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbusCallFailed;

impl std::fmt::Display for DbusCallFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DbusCallFailed")
    }
}

impl std::error::Error for DbusCallFailed {}

/// Formats the transient scope unit name for `pid`.
///
/// (Zig formats into a caller-provided buffer and asserts it is large
/// enough for any `u32`; the port returns a `String`.)
pub fn fmt_scope(pid: u32) -> String {
    format!("app-ghostty-surface-transient-{pid}.scope")
}

/// Builds the `(ssa(sv)a(sa(sv)))` parameters of `StartTransientUnit`.
fn start_transient_unit_parameters(name: &str, pid: u32, options: Options) -> glib::Variant {
    // Properties
    let mut properties: Vec<glib::Variant> = Vec::new();
    let property =
        |key: &str, value: glib::Variant| (key.to_owned(), value).to_variant();

    if let Some(value) = options.memory_high {
        properties.push(property("MemoryHigh", value.to_variant()));
    }

    if let Some(value) = options.tasks_max {
        properties.push(property("TasksMax", value.to_variant()));
    }

    // https://www.freedesktop.org/software/systemd/man/latest/systemd-oomd.service.html
    properties.push(property("ManagedOOMMemoryPressure", "kill".to_variant()));

    // PID to move into the unit
    let pids_value = glib::Variant::array_from_fixed_array(&[pid]);
    properties.push(property("PIDs", pids_value));

    let properties =
        glib::Variant::array_from_iter_with_type(glib::VariantTy::new("(sv)").unwrap(), properties);

    // Aux - unused but must be present
    let aux = glib::Variant::array_from_iter_with_type(
        glib::VariantTy::new("(sa(sv))").unwrap(),
        std::iter::empty::<glib::Variant>(),
    );

    glib::Variant::tuple_from_iter([
        name.to_variant(),
        "fail".to_variant(),
        properties,
        aux,
    ])
}

/// Create a transient systemd scope unit for the given process and
/// move the process into it.
pub fn create_scope(
    dbus: &gio::DBusConnection,
    pid: u32,
    options: Options,
) -> Result<(), DbusCallFailed> {
    // The unit name needs to be unique. We use the PID for this.
    let name = fmt_scope(pid);

    let value = start_transient_unit_parameters(&name, pid, options);

    let reply_type = glib::VariantTy::new("(o)").unwrap();

    match dbus.call_sync(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "StartTransientUnit",
        Some(&value),
        Some(reply_type),
        gio::DBusCallFlags::NONE,
        -1,
        None::<&gio::Cancellable>,
    ) {
        Ok(_reply) => Ok(()),
        Err(e) => {
            log::error!(
                target: "gtk_systemd_cgroup",
                "creating transient cgroup scope failed code={} err={}",
                e.code(),
                e.message(),
            );
            Err(DbusCallFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_name() {
        assert_eq!(fmt_scope(1234), "app-ghostty-surface-transient-1234.scope");
        assert_eq!(
            fmt_scope(u32::MAX),
            "app-ghostty-surface-transient-4294967295.scope"
        );
    }

    #[test]
    fn parameters_shape() {
        let v = start_transient_unit_parameters(
            "unit.scope",
            42,
            Options {
                memory_high: Some(1024),
                tasks_max: None,
            },
        );
        assert_eq!(v.type_().as_str(), "(ssa(sv)a(sa(sv)))");
        assert_eq!(
            v.print(true).as_str(),
            "('unit.scope', 'fail', [('MemoryHigh', <uint64 1024>), \
             ('ManagedOOMMemoryPressure', <'kill'>), ('PIDs', <[uint32 42]>)], \
             @a(sa(sv)) [])"
        );
    }
}
