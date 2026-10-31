//! Port of `src/datastruct/message_data.zig`.
//!
//! STATUS: ported
//!
//! Deviations: Zig picks the smallest integer that holds `small_size` for
//! the small length (`std.math.IntFittingRange`). Rust can't compute a type
//! from a const generic, so the length type is a third generic parameter
//! (`Len`, default `u8`) that must be able to hold `SMALL` (checked at
//! compile time). This keeps `MessageData<u8, 38>` at 40 bytes like the
//! Zig union. The `stable` variant holds a `&'static` slice (const data),
//! and the `alloc` variant owns a `Box<[Elem]>` freed on drop.

use crate::quirks::inline_assert as assert;

/// Integer types usable as the small-length type of [`MessageData`].
pub trait SmallLen: Copy + Default + std::fmt::Debug {
    const MAX: usize;
    fn from_usize(v: usize) -> Self;
    fn to_usize(self) -> usize;
}

macro_rules! impl_small_len {
    ($($t:ty),*) => {$(
        impl SmallLen for $t {
            const MAX: usize = <$t>::MAX as usize;
            #[inline(always)]
            fn from_usize(v: usize) -> Self { v as $t }
            #[inline(always)]
            fn to_usize(self) -> usize { self as usize }
        }
    )*};
}

impl_small_len!(u8, u16, u32, usize);

/// A small write where the data fits into this union size.
#[derive(Debug, Clone, Copy)]
pub struct Small<Elem: Copy + Default, const SMALL: usize, Len: SmallLen = u8> {
    pub data: [Elem; SMALL],
    pub len: Len,
}

impl<Elem: Copy + Default, const SMALL: usize, Len: SmallLen> Small<Elem, SMALL, Len> {
    pub const MAX: usize = SMALL;
}

impl<Elem: Copy + Default, const SMALL: usize, Len: SmallLen> Default for Small<Elem, SMALL, Len> {
    fn default() -> Self {
        Self {
            data: [Elem::default(); SMALL],
            len: Len::default(),
        }
    }
}

/// Creates a union that can be used to accommodate data that fit within an array,
/// are a stable pointer, or require deallocation. This is helpful for thread
/// messaging utilities.
#[derive(Debug, Clone)]
pub enum MessageData<Elem: Copy + Default + 'static, const SMALL: usize, Len: SmallLen = u8> {
    /// A small write where the data fits into this union size.
    Small(Small<Elem, SMALL, Len>),

    /// A stable pointer so we can just pass the slice directly through.
    /// This is useful i.e. for const data.
    Stable(&'static [Elem]),

    /// Allocated and freed when dropped. This should be rarely used.
    Alloc(Box<[Elem]>),
}

impl<Elem: Copy + Default + 'static, const SMALL: usize, Len: SmallLen>
    MessageData<Elem, SMALL, Len>
{
    const LEN_FITS: () = std::assert!(SMALL <= Len::MAX, "Len type too small for SMALL");

    /// Initializes the union for a given data type. This will
    /// attempt to fit into a small value if possible, otherwise
    /// will allocate and put into alloc.
    ///
    /// This can't and will never detect stable pointers.
    pub fn init(data: &[Elem]) -> Self {
        #[allow(clippy::let_unit_value)]
        let () = Self::LEN_FITS;

        // If it fits in our small request, do that.
        if data.len() <= SMALL {
            let mut buf = [Elem::default(); SMALL];
            buf[..data.len()].copy_from_slice(data);
            return Self::Small(Small {
                data: buf,
                len: Len::from_usize(data.len()),
            });
        }

        // Otherwise, allocate
        Self::Alloc(data.into())
    }

    /// Free any allocated data. Equivalent to dropping the value.
    pub fn deinit(self) {
        drop(self);
    }

    /// Returns a const slice of the data pointed to by this request.
    pub fn slice(&self) -> &[Elem] {
        match self {
            Self::Small(v) => {
                assert(v.len.to_usize() <= SMALL);
                &v.data[..v.len.to_usize()]
            }
            Self::Stable(v) => v,
            Self::Alloc(v) => v,
        }
    }

    pub fn is_small(&self) -> bool {
        matches!(self, Self::Small(_))
    }

    pub fn is_stable(&self) -> bool {
        matches!(self, Self::Stable(_))
    }

    pub fn is_alloc(&self) -> bool {
        matches!(self, Self::Alloc(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_data_init_small() {
        type Data = MessageData<u8, 10>;
        let input = b"hello!";
        let io = Data::init(input);
        assert!(io.is_small());
        assert_eq!(io.slice(), input);
    }

    #[test]
    fn message_data_init_alloc() {
        type Data = MessageData<u8, 10>;
        let mut input = [0u8; 7 * 100];
        for i in 0..100 {
            input[i * 7..][..7].copy_from_slice(b"hello! ");
        }

        let io = Data::init(&input);
        assert!(io.is_alloc());
        assert_eq!(io.slice(), &input[..]);
        io.deinit();
    }

    #[test]
    fn message_data_small_fits_non_u8_sized_data() {
        const LEN: usize = 500;
        type Data = MessageData<u8, LEN, u16>;
        let input = [b'X'; LEN];
        let io = Data::init(&input);
        assert!(io.is_small());
    }

    #[test]
    fn message_data_size() {
        // Not in the Zig file: termio relies on MessageData(u8, 38) keeping
        // its message at 40 bytes.
        assert_eq!(40, std::mem::size_of::<MessageData<u8, 38>>());
    }
}
