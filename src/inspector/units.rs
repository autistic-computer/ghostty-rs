//! Port of `src/inspector/units.zig`.
//!
//! STATUS: ported

pub fn to_kibi_bytes(bytes: usize) -> usize {
    bytes / 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kibi_bytes() {
        assert_eq!(to_kibi_bytes(0), 0);
        assert_eq!(to_kibi_bytes(1023), 0);
        assert_eq!(to_kibi_bytes(4096), 4);
    }
}
