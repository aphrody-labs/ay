//! Safe byte ↔ value conversions (`#![forbid(unsafe_code)]`), combining
//! `oven-sh/bun`'s `bun_pod` ergonomics with `google/zerocopy` compile-time layout proofs.
//!
//! Never use `core::mem::transmute` or `slice::from_raw_parts` on bytes. External, wire,
//! and JS-controlled buffers have arbitrary alignment: use [`read_prefix`],
//! [`vec_from_bytes`], or [`unaligned_slice`] rather than aligned slice casts.

pub use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, big_endian, little_endian,
};

/// View an initialized, immutable `#[repr(C)]`/`#[repr(transparent)]` value as raw bytes.
#[inline]
#[must_use]
pub fn bytes_of<T: IntoBytes + Immutable>(value: &T) -> &[u8] {
    value.as_bytes()
}

/// View a contiguous slice of `IntoBytes + Immutable` values as raw bytes without copying.
#[inline]
#[must_use]
pub fn slice_as_bytes<T: IntoBytes + Immutable>(slice: &[T]) -> &[u8] {
    slice.as_bytes()
}

/// Decode one `T` from the start of `bytes` regardless of alignment, returning `(value, rest)`.
#[inline]
#[must_use]
pub fn read_prefix<T: FromBytes + KnownLayout + Immutable>(bytes: &[u8]) -> Option<(T, &[u8])> {
    T::read_from_prefix(bytes).ok()
}

/// Decode a whole byte slice into `Vec<T>` regardless of alignment.
/// Returns `None` if `bytes.len()` is not an exact multiple of `size_of::<T>()` or on OOM.
#[must_use]
pub fn vec_from_bytes<T: FromBytes + KnownLayout + Immutable>(bytes: &[u8]) -> Option<Vec<T>> {
    let elem = core::mem::size_of::<T>();
    if elem == 0 || !bytes.len().is_multiple_of(elem) {
        return None;
    }
    let count = bytes.len() / elem;
    let mut out = Vec::new();
    out.try_reserve_exact(count).ok()?;
    let mut cursor = bytes;
    while !cursor.is_empty() {
        let (val, rest) = T::read_from_prefix(cursor).ok()?;
        out.push(val);
        cursor = rest;
    }
    Some(out)
}

/// Zero-copy view `&[u8] -> &[T]`. Returns `None` if `bytes` is misaligned or has trailing bytes.
#[inline]
#[must_use]
pub fn try_cast_slice<T: FromBytes + Immutable>(bytes: &[u8]) -> Option<&[T]> {
    <[T]>::ref_from_bytes(bytes).ok()
}

/// Zero-copy view `&[u8] -> &[T]` for `Unaligned` wire types (`align_of::<T>() == 1`).
/// Cannot fail due to alignment; returns `None` only on trailing partial elements.
#[inline]
#[must_use]
pub fn unaligned_slice<T: FromBytes + Immutable + Unaligned>(bytes: &[u8]) -> Option<&[T]> {
    <[T]>::ref_from_bytes(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
    struct WireHeader {
        magic: little_endian::U32,
        len: little_endian::U16,
        flags: little_endian::U16,
    }

    #[test]
    fn roundtrip_prefix_and_unaligned_buffers() {
        let hdr = WireHeader {
            magic: little_endian::U32::new(0x4150_4852),
            len: little_endian::U16::new(64),
            flags: little_endian::U16::new(3),
        };
        let raw = bytes_of(&hdr);
        assert_eq!(raw.len(), 8);

        // Shift by 1 byte to force misalignment (JS ArrayBuffer / wire framing scenario).
        let mut shifted = Vec::with_capacity(1 + raw.len() + 2);
        shifted.push(0xFF);
        shifted.extend_from_slice(raw);
        shifted.extend_from_slice(&[0xAA, 0xBB]);

        let unaligned = &shifted[1..];
        let (decoded, rest) = read_prefix::<WireHeader>(unaligned).expect("unaligned prefix");
        assert_eq!(decoded, hdr);
        assert_eq!(rest, &[0xAA, 0xBB]);

        // Aligned `try_cast_slice::<u32>` must reject a +1-shifted slice.
        assert!(try_cast_slice::<u32>(&shifted[1..9]).is_none());

        // `vec_from_bytes` and `unaligned_slice` handle arbitrary alignment safely.
        let vals = vec_from_bytes::<little_endian::U32>(&shifted[1..9]).expect("vec decode");
        assert_eq!(vals.len(), 2);
        assert_eq!(vals[0].get(), 0x4150_4852);

        let view = unaligned_slice::<little_endian::U32>(&shifted[1..9]).expect("unaligned slice");
        assert_eq!(view[0].get(), 0x4150_4852);
        assert!(unaligned_slice::<little_endian::U32>(&shifted[1..8]).is_none());
    }
}
