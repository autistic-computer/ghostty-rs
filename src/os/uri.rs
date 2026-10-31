//! Port of `src/os/uri.zig`.
//!
//! STATUS: ported
//!
//! Rust's `std` has no URI type, so this module also contains a port of the
//! parts of Zig's `std.Uri` that Ghostty relies on ([`Uri`], [`Component`],
//! [`Uri::parse`], [`Uri::parse_after_scheme`], [`percent_encode`]). Parsed
//! components borrow from the input text, exactly like Zig.

use std::fmt;
use std::io;

/// Zig: `std.Uri.Component`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component<'a> {
    /// Invalid characters in this component must be percent encoded
    /// before being printed as part of a URI.
    Raw(&'a str),
    /// This component is already percent-encoded, it can be printed
    /// directly as part of a URI.
    PercentEncoded(&'a str),
}

impl<'a> Component<'a> {
    pub const EMPTY: Component<'static> = Component::PercentEncoded("");

    pub fn is_empty(&self) -> bool {
        match self {
            Component::Raw(s) | Component::PercentEncoded(s) => s.is_empty(),
        }
    }

    /// The percent-encoded value (Zig: `.percent_encoded` field access).
    /// Panics if the component is raw.
    pub fn percent_encoded(&self) -> &'a str {
        match self {
            Component::PercentEncoded(s) => s,
            Component::Raw(_) => panic!("component is raw"),
        }
    }

    /// Returns the raw (decoded) value, decoding into `buffer` if
    /// necessary. Zig: `Component.toRaw`.
    pub fn to_raw<'b>(&self, buffer: &'b mut [u8]) -> Result<&'b [u8], NoSpaceLeft>
    where
        'a: 'b,
    {
        match *self {
            Component::Raw(raw) => {
                if raw.len() > buffer.len() {
                    return Err(NoSpaceLeft);
                }
                buffer[..raw.len()].copy_from_slice(raw.as_bytes());
                Ok(&buffer[..raw.len()])
            }
            Component::PercentEncoded(encoded) => {
                if encoded.len() > buffer.len() {
                    return Err(NoSpaceLeft);
                }
                buffer[..encoded.len()].copy_from_slice(encoded.as_bytes());
                let len = percent_decode_in_place(&mut buffer[..encoded.len()]);
                Ok(&buffer[..len])
            }
        }
    }
}

/// Zig: `error.NoSpaceLeft`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSpaceLeft;

/// Zig: `std.Uri.percentDecodeInPlace`. Invalid escapes are kept as-is.
pub fn percent_decode_in_place(buffer: &mut [u8]) -> usize {
    let mut dst = 0;
    let mut i = 0;
    while i < buffer.len() {
        if buffer[i] == b'%' && i + 2 < buffer.len() {
            let hi = (buffer[i + 1] as char).to_digit(16);
            let lo = (buffer[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                buffer[dst] = (hi * 16 + lo) as u8;
                dst += 1;
                i += 3;
                continue;
            }
        }
        buffer[dst] = buffer[i];
        dst += 1;
        i += 1;
    }
    dst
}

/// Write `raw` to `w`, percent encoding every byte for which `is_valid_char`
/// returns false. Zig: `std.Uri.Component.percentEncode`.
pub fn percent_encode<W: io::Write + ?Sized>(
    w: &mut W,
    raw: &[u8],
    is_valid_char: fn(u8) -> bool,
) -> io::Result<()> {
    let mut start = 0;
    for (index, &char) in raw.iter().enumerate() {
        if is_valid_char(char) {
            continue;
        }
        w.write_all(&raw[start..index])?;
        write!(w, "%{char:02X}")?;
        start = index + 1;
    }
    w.write_all(&raw[start..])
}

/// Zig: `std.Uri`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uri<'a> {
    pub scheme: &'a str,
    pub user: Option<Component<'a>>,
    pub password: Option<Component<'a>>,
    pub host: Option<Component<'a>>,
    pub port: Option<u16>,
    pub path: Component<'a>,
    pub query: Option<Component<'a>>,
    pub fragment: Option<Component<'a>>,
}

/// Zig: `std.Uri.ParseError || error{InvalidMacAddress}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    UnexpectedCharacter,
    InvalidFormat,
    InvalidPort,
    InvalidMacAddress,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ParseError {}

fn is_scheme_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.')
}

/// Zig: `std.fmt.parseInt(u16, s, 10)` (accepts a leading `+` and `_`
/// digit separators between digits).
fn parse_port(s: &str) -> Option<u16> {
    let digits = s.strip_prefix('+').unwrap_or(s);
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') {
        return None;
    }
    let mut value: u32 = 0;
    for c in digits.bytes() {
        if c == b'_' {
            continue;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (c - b'0') as u32;
        if value > u16::MAX as u32 {
            return None;
        }
    }
    Some(value as u16)
}

impl<'a> Uri<'a> {
    /// Parses the URI or returns an error. This function is not compliant,
    /// but is required to parse some forms of URIs in the wild, such as
    /// HTTP Location headers. The return value will contain strings
    /// pointing into the original `text`.
    pub fn parse_after_scheme(scheme: &'a str, text: &'a str) -> Result<Uri<'a>, ParseError> {
        let mut uri = Uri {
            scheme,
            user: None,
            password: None,
            host: None,
            port: None,
            path: Component::EMPTY,
            query: None,
            fragment: None,
        };
        let bytes = text.as_bytes();
        let mut i = 0usize;

        if text.starts_with("//") {
            'a: {
                i = bytes[2..]
                    .iter()
                    .position(|c| matches!(c, b'/' | b'?' | b'#'))
                    .map(|p| p + 2)
                    .unwrap_or(text.len());
                let authority = &text[2..i];
                if authority.is_empty() {
                    if !text[2..].starts_with('/') {
                        return Err(ParseError::InvalidFormat);
                    }
                    break 'a;
                }

                let mut start_of_host = 0usize;
                if let Some(index) = authority.find('@') {
                    start_of_host = index + 1;
                    let user_info = &authority[..index];

                    if let Some(idx) = user_info.find(':') {
                        uri.user = Some(Component::PercentEncoded(&user_info[..idx]));
                        // empty password is also "no password"
                        if idx < user_info.len() - 1 {
                            uri.password = Some(Component::PercentEncoded(&user_info[idx + 1..]));
                        }
                    } else {
                        uri.user = Some(Component::PercentEncoded(user_info));
                        uri.password = None;
                    }
                }

                // only possible if uri consists of only `userinfo@`
                if start_of_host >= authority.len() {
                    break 'a;
                }

                let mut end_of_host = authority.len();
                let ab = authority.as_bytes();

                // if we see `]` first without `@`
                if ab[start_of_host] == b']' {
                    return Err(ParseError::InvalidFormat);
                }

                if ab[start_of_host] == b'[' {
                    // IPv6
                    end_of_host = authority.rfind(']').ok_or(ParseError::InvalidFormat)?;
                    end_of_host += 1;

                    if let Some(index) = authority.rfind(':') {
                        // if not part of the V6 address field
                        if index >= end_of_host {
                            end_of_host = end_of_host.min(index);
                            uri.port = Some(
                                parse_port(&authority[index + 1..])
                                    .ok_or(ParseError::InvalidPort)?,
                            );
                        }
                    }
                } else if let Some(index) = authority.rfind(':') {
                    // if not part of the userinfo field
                    if index >= start_of_host {
                        end_of_host = end_of_host.min(index);
                        uri.port = Some(
                            parse_port(&authority[index + 1..]).ok_or(ParseError::InvalidPort)?,
                        );
                    }
                }

                if start_of_host >= end_of_host {
                    return Err(ParseError::InvalidFormat);
                }
                uri.host = Some(Component::PercentEncoded(
                    &authority[start_of_host..end_of_host],
                ));
            }
        }

        let path_start = i;
        i = bytes[path_start..]
            .iter()
            .position(|c| matches!(c, b'?' | b'#'))
            .map(|p| p + path_start)
            .unwrap_or(text.len());
        uri.path = Component::PercentEncoded(&text[path_start..i]);

        if i < text.len() && bytes[i] == b'?' {
            let query_start = i + 1;
            i = bytes[query_start..]
                .iter()
                .position(|&c| c == b'#')
                .map(|p| p + query_start)
                .unwrap_or(text.len());
            uri.query = Some(Component::PercentEncoded(&text[query_start..i]));
        }

        if i < text.len() {
            debug_assert_eq!(bytes[i], b'#');
            let fragment_start = i + 1;
            i = text.len();
            uri.fragment = Some(Component::PercentEncoded(&text[fragment_start..i]));
        }

        Ok(uri)
    }

    /// The return value will contain strings pointing into the original
    /// `text`. Each component that is provided will be non-`None`.
    /// Zig: `std.Uri.parse`.
    pub fn parse(text: &'a str) -> Result<Uri<'a>, ParseError> {
        let end = text
            .bytes()
            .position(|b| !is_scheme_char(b))
            .unwrap_or(text.len());
        // After the scheme, a ':' must appear.
        if end >= text.len() {
            return Err(ParseError::InvalidFormat);
        }
        if text.as_bytes()[end] != b':' {
            return Err(ParseError::UnexpectedCharacter);
        }
        Uri::parse_after_scheme(&text[..end], &text[end + 1..])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseOptions {
    /// Parse MAC addresses in the host component.
    ///
    /// This is useful when the "Private Wi-Fi address" is enabled on macOS,
    /// which sets the hostname to a rotating MAC address (12:34:56:ab:cd:ef).
    pub mac_address: bool,

    /// Return the full, raw, unencoded path string. Any query and fragment
    /// values will be return as part of the path instead of as distinct
    /// fields.
    pub raw_path: bool,
}

/// Byte offset of `sub` within `text`; `sub` must be a subslice of `text`.
fn offset_in(text: &str, sub: &str) -> usize {
    sub.as_ptr() as usize - text.as_ptr() as usize
}

/// Parses a URI from the given string.
///
/// This extends `Uri::parse` with some additional ParseOptions.
pub fn parse(text: &str, options: ParseOptions) -> Result<Uri<'_>, ParseError> {
    let mut uri = match Uri::parse(text) {
        Ok(uri) => uri,
        Err(err) => {
            // We can attempt to re-parse the text as a URI that has a MAC
            // address in its host field (which tripped up Uri::parse's port
            // parsing):
            //
            //      file://12:34:56:78:90:aa/path/to/file
            //                            ^^ InvalidPort
            //
            if err != ParseError::InvalidPort || !options.mac_address {
                return Err(err);
            }

            // We can assume that the initial Uri::parse already validated
            // the scheme, so we only need to find its bounds within the
            // string.
            let scheme_end = text.find("://").ok_or(ParseError::InvalidFormat)?;
            let scheme = &text[..scheme_end];

            // We similarly find the bounds of the host component by looking
            // for the first slash (/) after the scheme. This is all we need
            // for this case because the resulting slice can be
            // unambiguously determined to be a MAC address (or not).
            let host_start = scheme_end + "://".len();
            let host_end = text[host_start..]
                .find('/')
                .map(|p| p + host_start)
                .unwrap_or(text.len());
            let mac_address = &text[host_start..host_end];
            if !is_valid_mac_address(mac_address.as_bytes()) {
                return Err(ParseError::InvalidMacAddress);
            }

            // Parse the rest of the text (starting with the path component)
            // as a partial URI and then add our MAC address as its host
            // component.
            let mut uri = Uri::parse_after_scheme(scheme, &text[host_end..])?;
            uri.host = Some(Component::PercentEncoded(mac_address));
            uri
        }
    };

    // When MAC address parsing is enabled, we need to handle the case where
    // Uri::parse parsed the address's last octet as a numeric port number.
    // We use a few heuristics to identify this case (14 characters, 4
    // colons) and then "repair" the result by reassign the .host component
    // to the full MAC address and clearing the .port component.
    //
    //    12:34:56:78:90:99 -> [12:34:56:78:90, 99] -> 12:34:56:78:90:99
    //    (original host)      (parsed host + port)    (restored host)
    //
    'mac: {
        if !options.mac_address {
            break 'mac;
        }
        let Some(host) = uri.host else { break 'mac };
        let host = host.percent_encoded();
        if host.len() != 14 || host.bytes().filter(|&c| c == b':').count() != 4 {
            break 'mac;
        }

        let Some(port) = uri.port else { break 'mac };
        if port > 99 {
            break 'mac;
        }

        // Uri::parse returns slices pointing into the original text string.
        let host_start = offset_in(text, host);
        let path_start = offset_in(text, uri.path.percent_encoded());
        let mac_address = &text[host_start..path_start];
        if !is_valid_mac_address(mac_address.as_bytes()) {
            return Err(ParseError::InvalidMacAddress);
        }

        uri.host = Some(Component::PercentEncoded(mac_address));
        uri.port = None;
    }

    // When the raw_path option is active, return everything after the
    // authority (host) in the .path component, including any query and
    // fragment values.
    if options.raw_path {
        // Uri::parse returns slices pointing into the original text string.
        let path_start = offset_in(text, uri.path.percent_encoded());
        uri.path = Component::Raw(&text[path_start..]);
        uri.query = None;
        uri.fragment = None;
    }

    Ok(uri)
}

/// Checks if a string represents a valid MAC address, e.g. 12:34:56:ab:cd:ef.
fn is_valid_mac_address(s: &[u8]) -> bool {
    if s.len() != 17 {
        return false;
    }

    for (i, &c) in s.iter().enumerate() {
        if i % 3 == 2 {
            if c != b':' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac() -> ParseOptions {
        ParseOptions {
            mac_address: true,
            ..Default::default()
        }
    }

    #[test]
    fn parse_mac_address() {
        // Numeric MAC address without a port
        let uri1 = parse("file://00:12:34:56:78:90/path", mac()).unwrap();
        assert_eq!("file", uri1.scheme);
        assert_eq!("00:12:34:56:78:90", uri1.host.unwrap().percent_encoded());
        assert_eq!("/path", uri1.path.percent_encoded());
        assert_eq!(None, uri1.port);

        // Numeric MAC address with a port
        let uri2 = parse("file://00:12:34:56:78:90:999/path", mac()).unwrap();
        assert_eq!("file", uri2.scheme);
        assert_eq!("00:12:34:56:78:90", uri2.host.unwrap().percent_encoded());
        assert_eq!("/path", uri2.path.percent_encoded());
        assert_eq!(Some(999), uri2.port);

        // Alphabetic MAC address without a port
        let uri3 = parse("file://ab:cd:ef:ab:cd:ef/path", mac()).unwrap();
        assert_eq!("file", uri3.scheme);
        assert_eq!("ab:cd:ef:ab:cd:ef", uri3.host.unwrap().percent_encoded());
        assert_eq!("/path", uri3.path.percent_encoded());
        assert_eq!(None, uri3.port);

        // Alphabetic MAC address with a port
        let uri4 = parse("file://ab:cd:ef:ab:cd:ef:999/path", mac()).unwrap();
        assert_eq!("file", uri4.scheme);
        assert_eq!("ab:cd:ef:ab:cd:ef", uri4.host.unwrap().percent_encoded());
        assert_eq!("/path", uri4.path.percent_encoded());
        assert_eq!(Some(999), uri4.port);

        // Numeric MAC address without a path component
        let uri5 = parse("file://00:12:34:56:78:90", mac()).unwrap();
        assert_eq!("file", uri5.scheme);
        assert_eq!("00:12:34:56:78:90", uri5.host.unwrap().percent_encoded());
        assert!(uri5.path.is_empty());

        // Alphabetic MAC address without a path component
        let uri6 = parse("file://ab:cd:ef:ab:cd:ef", mac()).unwrap();
        assert_eq!("file", uri6.scheme);
        assert_eq!("ab:cd:ef:ab:cd:ef", uri6.host.unwrap().percent_encoded());
        assert!(uri6.path.is_empty());

        // Invalid MAC addresses
        assert_eq!(
            Err(ParseError::InvalidMacAddress),
            parse("file://zz:zz:zz:zz:zz:00/path", mac())
        );
        assert_eq!(
            Err(ParseError::InvalidMacAddress),
            parse("file://zz:zz:zz:zz:zz:zz/path", mac())
        );
    }

    #[test]
    fn parse_raw_path() {
        let text = "file://localhost/path??#fragment";
        let mut buf = [0u8; 256];

        let uri1 = parse(text, ParseOptions::default()).unwrap();
        assert_eq!("file", uri1.scheme);
        assert_eq!("localhost", uri1.host.unwrap().percent_encoded());
        assert_eq!(b"/path", uri1.path.to_raw(&mut buf).unwrap());
        assert_eq!("?", uri1.query.unwrap().percent_encoded());
        assert_eq!("fragment", uri1.fragment.unwrap().percent_encoded());

        let uri2 = parse(
            text,
            ParseOptions {
                raw_path: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!("file", uri2.scheme);
        assert_eq!("localhost", uri2.host.unwrap().percent_encoded());
        assert_eq!(b"/path??#fragment", uri2.path.to_raw(&mut buf).unwrap());
        assert_eq!(None, uri2.query);
        assert_eq!(None, uri2.fragment);

        let uri3 = parse(
            "file://localhost",
            ParseOptions {
                raw_path: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!("file", uri3.scheme);
        assert_eq!("localhost", uri3.host.unwrap().percent_encoded());
        assert!(uri3.path.is_empty());
        assert_eq!(None, uri3.query);
        assert_eq!(None, uri3.fragment);
    }

    #[test]
    fn is_valid_mac_address_test() {
        assert!(is_valid_mac_address(b"01:23:45:67:89:Aa"));
        assert!(is_valid_mac_address(b"Aa:Bb:Cc:Dd:Ee:Ff"));

        assert!(!is_valid_mac_address(b""));
        assert!(!is_valid_mac_address(b"00:23:45"));
        assert!(!is_valid_mac_address(b"00:23:45:Xx:Yy:Zz"));
        assert!(!is_valid_mac_address(b"01-23-45-67-89-Aa"));
        assert!(!is_valid_mac_address(b"01:23:45:67:89:Aa:Bb"));
    }

    #[test]
    fn std_uri_parse_basics() {
        let uri = Uri::parse("https://user:pw@[::1]:8080/a%20b?q=1#frag").unwrap();
        assert_eq!("https", uri.scheme);
        assert_eq!("user", uri.user.unwrap().percent_encoded());
        assert_eq!("pw", uri.password.unwrap().percent_encoded());
        assert_eq!("[::1]", uri.host.unwrap().percent_encoded());
        assert_eq!(Some(8080), uri.port);
        let mut buf = [0u8; 32];
        assert_eq!(b"/a b", uri.path.to_raw(&mut buf).unwrap());
        assert_eq!("q=1", uri.query.unwrap().percent_encoded());
        assert_eq!("frag", uri.fragment.unwrap().percent_encoded());

        assert_eq!(Err(ParseError::InvalidFormat), Uri::parse("nocolon"));
        assert_eq!(Err(ParseError::UnexpectedCharacter), Uri::parse("a b:c"));
        assert_eq!(Err(ParseError::InvalidPort), Uri::parse("http://host:x/"));
        let file = Uri::parse("file:///tmp/x").unwrap();
        assert_eq!(None, file.host);
        assert_eq!("/tmp/x", file.path.percent_encoded());
    }
}
