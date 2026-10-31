//! Port of `src/terminal/kitty/windows.zig`.
//!
//! STATUS: ported
//!
//! Windows path checks for the Kitty graphics protocol file and
//! temporary file mediums.
//!
//! On POSIX the file transmission blocklist is a few prefixes
//! (`/proc`, `/sys`, `/dev`) applied to the canonical path of the
//! opened file. Windows needs more care because some paths are
//! dangerous to even open: a UNC path makes this process resolve and
//! authenticate to the named SMB server, and the Win32 device and NT
//! object namespaces reach raw volumes and named pipes, where the
//! open or the read can block.
//!
//! Everything here is pure string logic, so it is tested on every host.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A UNC path such as `\\server\share`, in any mix of `\` and `/`.
    /// Opening it resolves the host name and authenticates to the
    /// server over SMB.
    UncPath,

    /// A Win32 device path (`\\.\`, `\\?\`) or an NT object path
    /// (`\??\`). These reach raw volumes, named pipes, and through
    /// `GLOBALROOT` any kernel object.
    DevicePath,

    /// A component names a reserved DOS device such as CON or COM1.
    /// Windows resolves these to the device from inside any directory.
    ReservedDeviceName,

    /// The canonical path of an opened file is not `X:\...`, so the
    /// file is not on a local volume with a drive letter. The canonical
    /// path of a file on an SMB share is a UNC path, for example.
    NotDriveAbsolute,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// Check a path supplied by a client before it is opened. This never
/// touches the filesystem. A plain local path of any shape passes:
/// drive-absolute, drive-relative, rooted, or relative. Whether it
/// resolves to a local regular file is decided after the open with
/// `check_canonical_path`.
pub fn check_path(path: &[u8]) -> Result<(), Error> {
    if path.len() >= 2 && is_sep(path[0]) && is_sep(path[1]) {
        // `\\?\` and `\\.\` are the Win32 device namespace. Anything
        // else after two separators is a server name.
        if path.len() >= 4 && (path[2] == b'?' || path[2] == b'.') && is_sep(path[3]) {
            return Err(Error::DevicePath);
        }
        return Err(Error::UncPath);
    }

    if path.len() >= 4 && is_sep(path[0]) && path[1] == b'?' && path[2] == b'?' && is_sep(path[3]) {
        return Err(Error::DevicePath);
    }

    for component in path.split(|&c| c == b'\\' || c == b'/') {
        if is_reserved_device_name(component) {
            return Err(Error::ReservedDeviceName);
        }
    }

    Ok(())
}

/// Check the canonical path of an opened file, as produced by
/// `GetFinalPathNameByHandle`. The file must be on a local volume with
/// a drive letter, and the checks of `check_path` apply as well.
pub fn check_canonical_path(path: &[u8]) -> Result<(), Error> {
    if !is_drive_absolute(path) {
        return Err(Error::NotDriveAbsolute);
    }
    check_path(path)
}

/// Returns true if `path` is a drive-absolute path (`C:\...`), the
/// only shape the canonical path of a file on a local volume with a
/// drive letter takes.
pub fn is_drive_absolute(path: &[u8]) -> bool {
    path.len() >= 3 && path[0].is_ascii_alphabetic() && path[1] == b':' && path[2] == b'\\'
}

/// Returns true if `c` is a Windows path separator.
fn is_sep(c: u8) -> bool {
    c == b'\\' || c == b'/'
}

/// Returns true if a path component names a reserved DOS device: CON,
/// PRN, AUX, NUL, COM0 to COM9, LPT0 to LPT9, and the superscript digit
/// variants.
fn is_reserved_device_name(component: &[u8]) -> bool {
    // The name ends at the extension or stream separator.
    let end = component
        .iter()
        .position(|&c| c == b'.' || c == b':')
        .unwrap_or(component.len());
    let mut stem = &component[..end];
    while let [rest @ .., b' '] = stem {
        stem = rest;
    }
    if stem.len() < 3 {
        return false;
    }

    let base = &stem[..3];
    let rest = &stem[3..];
    if rest.is_empty() {
        return base.eq_ignore_ascii_case(b"CON")
            || base.eq_ignore_ascii_case(b"PRN")
            || base.eq_ignore_ascii_case(b"AUX")
            || base.eq_ignore_ascii_case(b"NUL");
    }

    if !base.eq_ignore_ascii_case(b"COM") && !base.eq_ignore_ascii_case(b"LPT") {
        return false;
    }
    match rest.len() {
        1 => rest[0].is_ascii_digit(),
        // U+00B9, U+00B2, U+00B3 (¹ ² ³) encoded as WTF-8.
        2 => rest[0] == 0xC2 && (rest[1] == 0xB9 || rest[1] == 0xB2 || rest[1] == 0xB3),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_path_rejects_unc_paths() {
        let unc: &[&[u8]] = &[
            b"\\\\server\\share\\tty-graphics-protocol-image.data",
            b"//server/share/tty-graphics-protocol-image.data",
            b"\\/server/share/image.data",
            b"/\\server\\share\\image.data",
            b"\\\\.x\\share\\image.data",
            b"\\\\",
        ];
        for path in unc {
            assert_eq!(Err(Error::UncPath), check_path(path));
        }
    }

    #[test]
    fn check_path_rejects_device_and_nt_namespaces() {
        let device: &[&[u8]] = &[
            b"\\\\?\\UNC\\server\\share\\image.data",
            b"\\\\?\\C:\\Windows\\Temp\\image.data",
            b"//?/C:/Windows/Temp/image.data",
            b"\\\\.\\C:\\Windows\\Temp\\image.data",
            b"\\\\.\\pipe\\image",
            b"\\\\.\\PhysicalDrive0",
            b"\\\\?\\GLOBALROOT\\Device\\HarddiskVolume1\\image.data",
            b"//?/GLOBALROOT/Device/HarddiskVolume1/image.data",
            b"\\??\\C:\\Windows\\Temp\\image.data",
            b"\\??\\Device\\HarddiskVolume1\\image.data",
            b"\\??\\UNC\\server\\share\\image.data",
            b"/??/C:/image.data",
        ];
        for path in device {
            assert_eq!(Err(Error::DevicePath), check_path(path));
        }
    }

    #[test]
    fn check_path_accepts_plain_local_paths() {
        let accepted: &[&[u8]] = &[
            b"C:\\Windows\\Temp\\tty-graphics-protocol-image.data",
            b"C:/Windows/Temp/tty-graphics-protocol-image.data",
            b"c:\\image.data",
            b"C:image.data",
            b"\\Windows\\Temp\\image.data",
            b"/Windows/Temp/image.data",
            b"image.data",
            b".zig-cache\\tmp\\image.data",
            b"C:\\Device\\HarddiskVolume1\\image.data",
            b"C:\\GLOBALROOT\\image.data",
            b"C:\\dir\\console.png",
            b"\\?\\image.data",
            b"",
        ];
        for path in accepted {
            check_path(path).unwrap();
        }
    }

    #[test]
    fn check_path_rejects_reserved_device_names_in_any_component() {
        let reserved: &[&[u8]] = &[
            b"CON",
            b"con",
            b"CON.png",
            b"CON.tar.gz",
            b"CON.",
            b"CON  ",
            b"CON .png",
            b"NUL:stream",
            b"PRN",
            b"AUX",
            b"nul",
            b"COM1",
            b"COM0",
            b"COM9.rgba",
            b"com5",
            b"LPT1",
            b"LPT9",
            b"lpt0.png",
            b"COM\xC2\xB9",
            b"LPT\xC2\xB3.png",
            b"C:\\Windows\\Temp\\NUL.rgba",
            b"C:\\CON\\image.data",
            b"dir/aux/image.data",
            b"C:/Windows/Temp/com1",
            b"..\\PRN",
        ];
        for path in reserved {
            assert_eq!(Err(Error::ReservedDeviceName), check_path(path));
        }

        let plain: &[&[u8]] = &[
            b"CONSOLE",
            b"CON1",
            b"xCON",
            b" CON",
            b"COM",
            b"COM10",
            b"COMA",
            b"COM\xC2\xB9\xC2\xB9",
            b"COM\xC2\xB0",
            b"LPT",
            b"LPT.png",
            b"conx.png",
            b"C:",
            b"C:\\",
            b"image.data",
            b"C:\\Windows\\Temp\\tty-graphics-protocol-image.data",
            b"C:\\Windows\\Temp\\console.png",
            b"",
        ];
        for path in plain {
            check_path(path).unwrap();
        }
    }

    #[test]
    fn check_canonical_path_requires_a_drive_absolute_local_path() {
        check_canonical_path(b"C:\\Windows\\Temp\\image.data").unwrap();
        check_canonical_path(b"c:\\image.data").unwrap();

        // Shapes GetFinalPathNameByHandle produces for files that are not
        // on a local drive-letter volume.
        assert_eq!(
            Err(Error::NotDriveAbsolute),
            check_canonical_path(b"\\\\server\\share\\image.data")
        );
        assert_eq!(
            Err(Error::NotDriveAbsolute),
            check_canonical_path(b"\\\\?\\UNC\\server\\share\\image.data")
        );
        assert_eq!(
            Err(Error::NotDriveAbsolute),
            check_canonical_path(
                b"\\\\?\\Volume{383da0b0-717f-41b6-8c36-00500992b58d}\\image.data"
            )
        );

        // The raw path checks still apply to the canonical form.
        assert_eq!(
            Err(Error::ReservedDeviceName),
            check_canonical_path(b"C:\\Windows\\Temp\\CON")
        );
    }

    #[test]
    fn is_drive_absolute_test() {
        assert!(is_drive_absolute(b"C:\\Windows\\Temp\\image.data"));
        assert!(is_drive_absolute(b"c:\\image.data"));
        assert!(is_drive_absolute(b"Z:\\"));

        assert!(!is_drive_absolute(b"\\\\server\\share\\image.data"));
        assert!(!is_drive_absolute(b"UNC\\server\\share\\image.data"));
        assert!(!is_drive_absolute(b"C:/Windows/image.data"));
        assert!(!is_drive_absolute(b"C:image.data"));
        assert!(!is_drive_absolute(b"\\Windows\\image.data"));
        assert!(!is_drive_absolute(b"1:\\image.data"));
        assert!(!is_drive_absolute(b"C:"));
        assert!(!is_drive_absolute(b""));
    }
}
