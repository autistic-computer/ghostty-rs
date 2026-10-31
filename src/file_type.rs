//! Port of `src/file_type.zig`.
//!
//! STATUS: ported

struct TypeDetails {
    typ: FileType,
    sigs: &'static [&'static [Option<u8>]],
    exts: &'static [&'static str],
}

macro_rules! sig {
    (@b null) => { None };
    (@b $b:expr) => { Some($b) };
    ($($b:tt),* $(,)?) => { &[$(sig!(@b $b)),*] };
}

const TYPE_DETAILS: &[TypeDetails] = &[
    TypeDetails {
        typ: FileType::Jpeg,
        sigs: &[
            sig![0xFF, 0xD8, 0xFF, 0xDB],
            sig![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01],
            sig![0xFF, 0xD8, 0xFF, 0xEE],
            sig![0xFF, 0xD8, 0xFF, 0xE1, null, null, 0x45, 0x78, 0x69, 0x66, 0x00, 0x00],
            sig![0xFF, 0xD8, 0xFF, 0xE0],
        ],
        exts: &[".jpg", ".jpeg", ".jfif"],
    },
    TypeDetails {
        typ: FileType::Png,
        sigs: &[sig![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]],
        exts: &[".png"],
    },
    TypeDetails {
        typ: FileType::Gif,
        sigs: &[
            sig![b'G', b'I', b'F', b'8', b'7', b'a'],
            sig![b'G', b'I', b'F', b'8', b'9', b'a'],
        ],
        exts: &[".gif"],
    },
    TypeDetails {
        typ: FileType::Bmp,
        sigs: &[sig![b'B', b'M']],
        exts: &[".bmp"],
    },
    TypeDetails {
        typ: FileType::Qoi,
        sigs: &[sig![b'q', b'o', b'i', b'f']],
        exts: &[".qoi"],
    },
    TypeDetails {
        typ: FileType::Webp,
        sigs: &[sig![
            0x52, 0x49, 0x46, 0x46, null, null, null, null, 0x57, 0x45, 0x42, 0x50
        ]],
        exts: &[".webp"],
    },
];

/// This is a helper for detecting file types based on magic bytes.
///
/// Ref: https://en.wikipedia.org/wiki/List_of_file_signatures
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileType {
    /// JPEG image file.
    Jpeg,

    /// PNG image file.
    Png,

    /// GIF image file.
    Gif,

    /// BMP image file.
    Bmp,

    /// QOI image file.
    Qoi,

    /// WebP image file.
    Webp,

    /// Unknown file format.
    Unknown,
}

impl FileType {
    /// Detect file type based on the magic bytes
    /// at the start of the provided file contents.
    pub fn detect(contents: &[u8]) -> FileType {
        for typ in TYPE_DETAILS {
            for signature in typ.sigs {
                if contents.len() >= signature.len() {
                    let matches = contents[..signature.len()]
                        .iter()
                        .zip(signature.iter())
                        .all(|(f, sig)| match sig {
                            Some(s) => f == s,
                            None => true,
                        });
                    if matches {
                        return typ.typ;
                    }
                }
            }
        }
        FileType::Unknown
    }

    /// Guess file type from its extension.
    pub fn guess_from_extension(extension: &[u8]) -> FileType {
        for typ in TYPE_DETAILS {
            for ext in typ.exts {
                if extension.eq_ignore_ascii_case(ext.as_bytes()) {
                    return typ.typ;
                }
            }
        }
        FileType::Unknown
    }

    /// The Zig tag name of this file type.
    pub fn as_str(self) -> &'static str {
        match self {
            FileType::Jpeg => "jpeg",
            FileType::Png => "png",
            FileType::Gif => "gif",
            FileType::Bmp => "bmp",
            FileType::Qoi => "qoi",
            FileType::Webp => "webp",
            FileType::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Zig file has no tests; these exercise the signature tables.
    #[test]
    fn detect_and_extension() {
        assert_eq!(FileType::Png, FileType::detect(b"\x89PNG\r\n\x1a\nrest"));
        assert_eq!(FileType::Gif, FileType::detect(b"GIF89a..."));
        assert_eq!(
            FileType::Jpeg,
            FileType::detect(b"\xFF\xD8\xFF\xE1\x12\x34Exif\x00\x00")
        );
        assert_eq!(
            FileType::Webp,
            FileType::detect(b"RIFF\x01\x02\x03\x04WEBPVP8 ")
        );
        assert_eq!(FileType::Unknown, FileType::detect(b"B"));
        assert_eq!(FileType::Bmp, FileType::detect(b"BM"));
        assert_eq!(FileType::Jpeg, FileType::guess_from_extension(b".JPEG"));
        assert_eq!(FileType::Qoi, FileType::guess_from_extension(b".qoi"));
        assert_eq!(FileType::Unknown, FileType::guess_from_extension(b".txt"));
    }
}
