//! Port of `src/terminal/charsets.zig`.
//!
//! STATUS: ported

/// The available charset slots for a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Slots {
    G0 = 0,
    G1 = 1,
    G2 = 2,
    G3 = 3,
}

impl Slots {
    /// All slots in declaration order.
    pub const ALL: [Slots; 4] = [Slots::G0, Slots::G1, Slots::G2, Slots::G3];
}

/// The name of the active slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ActiveSlot {
    GL = 0,
    GR = 1,
}

/// The list of supported character sets and their associated tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Charset {
    Utf8 = 0,
    Ascii = 1,
    British = 2,
    DecSpecial = 3,
}

impl Charset {
    /// All charsets in declaration order.
    pub const ALL: [Charset; 4] = [
        Charset::Utf8,
        Charset::Ascii,
        Charset::British,
        Charset::DecSpecial,
    ];
}

/// The table for the given charset. This returns a pointer to a
/// slice that is guaranteed to be 255 chars that can be used to map
/// ASCII to the given charset.
#[inline]
pub fn table(set: Charset) -> &'static [u16] {
    match set {
        Charset::British => &BRITISH,
        Charset::DecSpecial => &DEC_SPECIAL,

        // utf8 is not a table, callers should double-check if the
        // charset is utf8 and NOT use tables.
        Charset::Utf8 => unreachable!(),

        // recommended that callers just map ascii directly but we can
        // support a table
        Charset::Ascii => &ASCII,
    }
}

/// Just a basic c => c ascii table
static ASCII: [u16; TABLE_LEN] = init_table();

/// https://vt100.net/docs/vt220-rm/chapter2.html
static BRITISH: [u16; TABLE_LEN] = {
    let mut tbl = init_table();
    tbl[0x23] = 0x00a3;
    tbl
};

/// https://en.wikipedia.org/wiki/DEC_Special_Graphics
static DEC_SPECIAL: [u16; TABLE_LEN] = {
    let mut tbl = init_table();
    tbl[0x60] = 0x25C6;
    tbl[0x61] = 0x2592;
    tbl[0x62] = 0x2409;
    tbl[0x63] = 0x240C;
    tbl[0x64] = 0x240D;
    tbl[0x65] = 0x240A;
    tbl[0x66] = 0x00B0;
    tbl[0x67] = 0x00B1;
    tbl[0x68] = 0x2424;
    tbl[0x69] = 0x240B;
    tbl[0x6a] = 0x2518;
    tbl[0x6b] = 0x2510;
    tbl[0x6c] = 0x250C;
    tbl[0x6d] = 0x2514;
    tbl[0x6e] = 0x253C;
    tbl[0x6f] = 0x23BA;
    tbl[0x70] = 0x23BB;
    tbl[0x71] = 0x2500;
    tbl[0x72] = 0x23BC;
    tbl[0x73] = 0x23BD;
    tbl[0x74] = 0x251C;
    tbl[0x75] = 0x2524;
    tbl[0x76] = 0x2534;
    tbl[0x77] = 0x252C;
    tbl[0x78] = 0x2502;
    tbl[0x79] = 0x2264;
    tbl[0x7a] = 0x2265;
    tbl[0x7b] = 0x03C0;
    tbl[0x7c] = 0x2260;
    tbl[0x7d] = 0x00A3;
    tbl[0x7e] = 0x00B7;
    tbl
};

/// Our table length is 256 so we can contain all ASCII chars.
const TABLE_LEN: usize = u8::MAX as usize + 1;

/// Creates a table that maps ASCII to ASCII as a getting started point.
const fn init_table() -> [u16; TABLE_LEN] {
    let mut result = [0u16; TABLE_LEN];
    let mut i: usize = 0;
    while i < TABLE_LEN {
        result[i] = i as u16;
        i += 1;
    }
    assert!(i == TABLE_LEN);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_lengths() {
        for set in Charset::ALL {
            // utf8 has no table
            if set == Charset::Utf8 {
                continue;
            }

            let tbl = table(set);

            // Yes, I could use `TABLE_LEN` here, but I want to explicitly use a
            // hardcoded constant so that if there are miscompilations or a comptime
            // issue, we catch it.
            assert_eq!(256usize, tbl.len());
        }
    }
}
