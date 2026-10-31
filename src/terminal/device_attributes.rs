//! Port of `src/terminal/device_attributes.zig`.
//!
//! STATUS: ported

use std::io::Write;

/// The device attribute request type (CSI c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Req {
    /// Blank
    Primary = 0,
    /// >
    Secondary = 1,
    /// =
    Tertiary = 2,
}

/// Response data for all device attribute queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attributes {
    /// Reply to CSI c (DA1).
    pub primary: Primary,

    /// Reply to CSI > c (DA2).
    pub secondary: Secondary,

    /// Reply to CSI = c (DA3).
    pub tertiary: Tertiary,
}

impl Attributes {
    /// Encode the response for the given request type into the writer.
    pub fn encode<W: Write + ?Sized>(&self, req: Req, writer: &mut W) -> std::io::Result<()> {
        match req {
            Req::Primary => self.primary.encode(writer),
            Req::Secondary => self.secondary.encode(writer),
            Req::Tertiary => self.tertiary.encode(writer),
        }
    }
}

/// Primary device attributes (DA1).
///
/// Response format: CSI ? Pp ; Ps... c
/// where Pp is the conformance level and Ps are feature flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Primary {
    /// Conformance level sent as the first parameter.
    pub conformance_level: ConformanceLevel,

    /// Optional feature attributes.
    pub features: &'static [Feature],
}

impl Default for Primary {
    fn default() -> Self {
        Self {
            conformance_level: ConformanceLevel::VT220,
            features: &[Feature::ANSI_COLOR],
        }
    }
}

/// DA1 feature attribute codes (`Primary.Feature`).
///
/// Non-exhaustive: any u16 value is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Feature(pub u16);

impl Feature {
    pub const COLUMNS_132: Feature = Feature(1);
    pub const PRINTER: Feature = Feature(2);
    pub const REGIS: Feature = Feature(3);
    pub const SIXEL: Feature = Feature(4);
    pub const SELECTIVE_ERASE: Feature = Feature(6);
    pub const USER_DEFINED_KEYS: Feature = Feature(8);
    pub const NATIONAL_REPLACEMENT: Feature = Feature(9);
    pub const TECHNICAL_CHARACTERS: Feature = Feature(15);
    pub const LOCATOR: Feature = Feature(16);
    pub const TERMINAL_STATE: Feature = Feature(17);
    pub const WINDOWING: Feature = Feature(18);
    pub const HORIZONTAL_SCROLLING: Feature = Feature(21);
    pub const ANSI_COLOR: Feature = Feature(22);
    pub const RECTANGULAR_EDITING: Feature = Feature(28);
    pub const ANSI_TEXT_LOCATOR: Feature = Feature(29);
    pub const CLIPBOARD: Feature = Feature(52);
}

impl Primary {
    /// Encode the primary DA response into the writer.
    pub fn encode<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        write!(writer, "\x1b[?{}", self.conformance_level.0)?;
        for feature in self.features {
            write!(writer, ";{}", feature.0)?;
        }
        writer.write_all(b"c")
    }
}

/// Secondary device attributes (DA2).
///
/// Response format: CSI > Pp ; Pv ; Pc c
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Secondary {
    /// Terminal type identifier (Pp parameter from secondary DA response).
    pub device_type: DeviceType,

    /// Firmware/patch version number.
    pub firmware_version: u16,

    /// ROM cartridge registration number. Always 0 for emulators.
    pub rom_cartridge: u16,
}

impl Default for Secondary {
    fn default() -> Self {
        Self {
            device_type: DeviceType::VT220,
            firmware_version: 0,
            rom_cartridge: 0,
        }
    }
}

impl Secondary {
    /// Encode the secondary DA response into the writer.
    pub fn encode<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        write!(
            writer,
            "\x1b[>{};{};{}c",
            self.device_type.0, self.firmware_version, self.rom_cartridge,
        )
    }
}

/// Tertiary device attributes (DA3).
///
/// Response format: DCS ! | D...D ST
/// where D...D is the unit ID as hex digits (DECRPTUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tertiary {
    /// Unit ID (DECRPTUI). Encoded as 8 uppercase hex digits.
    /// Meaningless for emulators nowadays. The actual DEC manuals
    /// appear to split this into two 16-bit fields but since there
    /// is no practical usage I know if I'm simplifying this.
    pub unit_id: u32,
}

impl Tertiary {
    /// Encode the tertiary DA response into the writer.
    pub fn encode<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        write!(writer, "\x1bP!|{:08X}\x1b\\", self.unit_id)
    }
}

/// Conformance level reported as the first parameter (Pp) in the
/// primary device attributes (DA1) response.
///
/// Non-exhaustive: any u16 value is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ConformanceLevel(pub u16);

impl ConformanceLevel {
    // VT100-series have per-model values.
    pub const VT100: ConformanceLevel = ConformanceLevel(1);
    pub const VT132: ConformanceLevel = ConformanceLevel(4);
    pub const VT102: ConformanceLevel = ConformanceLevel(6);
    pub const VT131: ConformanceLevel = ConformanceLevel(7);
    pub const VT125: ConformanceLevel = ConformanceLevel(12);

    // VT200+ use 60 + decTerminalID/100.
    /// Level 2 conformance (VT200 series, e.g. VT220, VT240).
    pub const LEVEL_2: ConformanceLevel = ConformanceLevel(62);
    /// Level 3 conformance (VT300 series, e.g. VT320, VT340).
    pub const LEVEL_3: ConformanceLevel = ConformanceLevel(63);
    /// Level 4 conformance (VT400 series, e.g. VT420).
    pub const LEVEL_4: ConformanceLevel = ConformanceLevel(64);
    /// Level 5 conformance (VT500 series, e.g. VT510, VT520, VT525).
    pub const LEVEL_5: ConformanceLevel = ConformanceLevel(65);

    pub const VT101: ConformanceLevel = ConformanceLevel::VT100;
    pub const VT220: ConformanceLevel = ConformanceLevel::LEVEL_2;
    pub const VT240: ConformanceLevel = ConformanceLevel::LEVEL_2;
    pub const VT320: ConformanceLevel = ConformanceLevel::LEVEL_3;
    pub const VT340: ConformanceLevel = ConformanceLevel::LEVEL_3;
    pub const VT420: ConformanceLevel = ConformanceLevel::LEVEL_4;
    pub const VT510: ConformanceLevel = ConformanceLevel::LEVEL_5;
    pub const VT520: ConformanceLevel = ConformanceLevel::LEVEL_5;
    pub const VT525: ConformanceLevel = ConformanceLevel::LEVEL_5;
}

/// Terminal type identifier reported as the Pp parameter in the
/// secondary device attributes (DA2) response. Values correspond
/// to the decTerminalID resource in xterm.
///
/// Non-exhaustive: any u16 value is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct DeviceType(pub u16);

impl DeviceType {
    pub const VT100: DeviceType = DeviceType(0);
    pub const VT220: DeviceType = DeviceType(1);
    pub const VT240: DeviceType = DeviceType(2);
    pub const VT330: DeviceType = DeviceType(18);
    pub const VT340: DeviceType = DeviceType(19);
    pub const VT320: DeviceType = DeviceType(24);
    pub const VT382: DeviceType = DeviceType(32);
    pub const VT420: DeviceType = DeviceType(41);
    pub const VT510: DeviceType = DeviceType(61);
    pub const VT520: DeviceType = DeviceType(64);
    pub const VT525: DeviceType = DeviceType(65);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(f: impl FnOnce(&mut &mut [u8]) -> std::io::Result<()>) -> Vec<u8> {
        let mut buf = [0u8; 64];
        let mut writer: &mut [u8] = &mut buf;
        f(&mut writer).unwrap();
        let n = 64 - writer.len();
        buf[..n].to_vec()
    }

    #[test]
    fn primary_default() {
        let out = encoded(|w| Primary::default().encode(w));
        assert_eq!(b"\x1b[?62;22c".as_slice(), out.as_slice());
    }

    #[test]
    fn primary_with_clipboard() {
        let out = encoded(|w| {
            Primary {
                features: &[Feature::ANSI_COLOR, Feature::CLIPBOARD],
                ..Default::default()
            }
            .encode(w)
        });
        assert_eq!(b"\x1b[?62;22;52c".as_slice(), out.as_slice());
    }

    #[test]
    fn primary_with_multiple_features() {
        let out = encoded(|w| {
            Primary {
                conformance_level: ConformanceLevel::VT420,
                features: &[
                    Feature::COLUMNS_132,
                    Feature::SELECTIVE_ERASE,
                    Feature::ANSI_COLOR,
                ],
            }
            .encode(w)
        });
        assert_eq!(b"\x1b[?64;1;6;22c".as_slice(), out.as_slice());
    }

    #[test]
    fn primary_no_features() {
        let out = encoded(|w| {
            Primary {
                conformance_level: ConformanceLevel::VT100,
                features: &[],
            }
            .encode(w)
        });
        assert_eq!(b"\x1b[?1c".as_slice(), out.as_slice());
    }

    #[test]
    fn secondary_default() {
        let out = encoded(|w| Secondary::default().encode(w));
        assert_eq!(b"\x1b[>1;0;0c".as_slice(), out.as_slice());
    }

    #[test]
    fn tertiary_default() {
        let out = encoded(|w| Tertiary::default().encode(w));
        assert_eq!(b"\x1bP!|00000000\x1b\\".as_slice(), out.as_slice());
    }

    #[test]
    fn tertiary_custom_unit_id() {
        let out = encoded(|w| Tertiary { unit_id: 0xAABBCCDD }.encode(w));
        assert_eq!(b"\x1bP!|AABBCCDD\x1b\\".as_slice(), out.as_slice());
    }
}
