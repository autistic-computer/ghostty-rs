//! Port of `src/apprt/gtk/portal.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "portal/open_uri.rs"]
pub mod open_uri;
// @generated-mods-end

pub use self::open_uri::OpenUri as OpenURI;

pub const TOKEN_HEX_LEN: usize = std::mem::size_of::<usize>() * 2;

/// `[token_hex_len + 1]u8` in Zig (room for the NUL); the port formats into
/// a `String`.
pub type TokenBuffer = [u8; TOKEN_HEX_LEN + 1];

/// Generate a token suitable for use in requests to the XDG Desktop Portal
pub fn generate_token() -> usize {
    getrandom::u64().expect("system RNG unavailable") as usize
}

/// Format a request token consistently for use in portal object paths and payloads.
pub fn format_token(token: usize) -> String {
    format!("{token:0>width$x}", width = TOKEN_HEX_LEN)
}

/// `error{NoDBusUniqueName}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoDBusUniqueName;

impl std::fmt::Display for NoDBusUniqueName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NoDBusUniqueName")
    }
}

impl std::error::Error for NoDBusUniqueName {}

/// Get the XDG portal request path for the current Ghostty instance.
///
/// See https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html
/// for the protocol of the Request interface.
pub fn get_request_path(
    dbus: &gio::DBusConnection,
    token: usize,
) -> Result<String, NoDBusUniqueName> {
    // Get the unique name from D-Bus and strip the leading `:`
    let unique_name = dbus.unique_name().ok_or(NoDBusUniqueName)?;
    let unique_name = unique_name.as_str();
    let unique_name = unique_name.get(1..).unwrap_or("");

    Ok(build_request_path(unique_name, token))
}

/// Build the XDG portal request path for given unique name and token.
fn build_request_path(unique_name: &str, token: usize) -> String {
    let token_string = format_token(token);

    let object_path = [
        "/org/freedesktop/portal/desktop/request",
        unique_name,
        &token_string,
    ]
    .join("/");

    // Sanitize the unique name by replacing every `.` with `_`. In effect, this
    // will turn a unique name like `1.192` into `1_192`.
    // This sounds arbitrary, but it's part of the Request protocol.
    object_path.replace('.', "_")
}

/// Try and parse the token out of a request path.
pub fn parse_request_path_token(request_path: &str) -> Option<usize> {
    let index = request_path.rfind('/')?;
    let token = &request_path[index + 1..];
    // `std.fmt.parseUnsigned` rejects a sign; `from_str_radix` accepts `+`.
    if token.starts_with('+') {
        return None;
    }
    usize::from_str_radix(token, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_token_pads_to_fixed_width() {
        let token = format_token(0x42);

        assert_eq!(TOKEN_HEX_LEN, token.len());
        assert_eq!("0000000000000042", token);
    }

    #[test]
    fn build_request_path_() {
        let path = build_request_path("1.42", 0x75af01a79c6fea34);
        assert_eq!(
            "/org/freedesktop/portal/desktop/request/1_42/75af01a79c6fea34",
            path,
        );
    }

    #[test]
    fn build_request_path_pads_token() {
        let path = build_request_path("1.42", 0x42);

        assert_eq!(
            "/org/freedesktop/portal/desktop/request/1_42/0000000000000042",
            path,
        );
    }

    #[test]
    fn parse_request_path_token_() {
        assert_eq!(
            Some(0x75af01a79c6fea34),
            parse_request_path_token(
                "/org/freedesktop/portal/desktop/request/1_42/75af01a79c6fea34"
            )
        );
        assert_eq!(
            None,
            parse_request_path_token(
                "/org/freedesktop/portal/desktop/request/1_42/75af01a79c6fGa34"
            )
        );
        assert_eq!(None, parse_request_path_token("75af01a79c6fea34"));
    }
}
