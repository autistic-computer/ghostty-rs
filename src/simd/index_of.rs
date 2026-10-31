//! Port of `src/simd/index_of.zig`.
//!
//! STATUS: ported
//!
//! The Zig SIMD build calls `index_of.cpp` (Highway); here the vectorized
//! search is provided by the `memchr` crate. Results are identical.

pub fn index_of(input: &[u8], needle: u8) -> Option<usize> {
    memchr::memchr(needle, input)
}

#[allow(dead_code)]
fn index_of_scalar(input: &[u8], needle: u8) -> Option<usize> {
    input.iter().position(|&b| b == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_of_test() {
        assert!(index_of(b"hello", b' ').is_none());
        assert_eq!(2usize, index_of(b"hi lo", b' ').unwrap());
        assert_eq!(
            5usize,
            index_of(
                b"XXXXX XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX\nXXXXXXXXXXXX XXXXXXXXXXX XXXXXXXXXXXXXXX",
                b' '
            )
            .unwrap()
        );
        assert_eq!(
            53usize,
            index_of(
                b"XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX\nXXXXXXXXXXXX XXXXXXXXXXX XXXXXXXXXXXXXXX",
                b' '
            )
            .unwrap()
        );
    }

    #[test]
    fn index_of_matches_scalar() {
        let mut buf = vec![b'a'; 300];
        for i in 0..buf.len() {
            buf[i] = b' ';
            for len in [i, i + 1, buf.len()] {
                assert_eq!(index_of_scalar(&buf[..len], b' '), index_of(&buf[..len], b' '));
            }
            buf[i] = b'a';
        }
    }
}
