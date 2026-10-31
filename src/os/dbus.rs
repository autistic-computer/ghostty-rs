//! Port of `src/os/dbus.zig`.
//!
//! STATUS: ported

/// Returns true if the program was launched by D-Bus activation.
///
/// On Linux GTK, this returns true if the program was launched using D-Bus
/// activation. It will return false if Ghostty was launched any other way.
///
/// For other platforms and app runtimes, this returns false.
pub fn launched_by_dbus_activation() -> bool {
    if cfg!(target_os = "linux") {
        // On Linux, D-Bus activation sets `DBUS_STARTER_ADDRESS` and
        // `DBUS_STARTER_BUS_TYPE`. If these environment variables are present
        // (no matter the value) we were launched by D-Bus activation.
        std::env::var_os("DBUS_STARTER_ADDRESS").is_some()
            && std::env::var_os("DBUS_STARTER_BUS_TYPE").is_some()
    } else {
        // No other system supports D-Bus so always return false.
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn launched_by_dbus_activation_matches_env() {
        let expected = cfg!(target_os = "linux")
            && std::env::var_os("DBUS_STARTER_ADDRESS").is_some()
            && std::env::var_os("DBUS_STARTER_BUS_TYPE").is_some();
        assert_eq!(expected, super::launched_by_dbus_activation());
    }
}
