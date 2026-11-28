//! Zero-copy binary `depot.lock` index (`DepotLockHeader` 24B + `DepotLockEntry` 94B) with Bun Wyhash integrity (`#![forbid(unsafe_code)]`).

use serde::{Deserialize, Serialize};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::ir::{ActionPlane, DepotError, Ecosystem};

pub const DEPOT_LOCK_MAGIC: [u8; 4] = *b"AYDL";
pub const DEPOT_LOCK_VERSION: u16 = 1;

/// 24-byte aligned-1 zero-copy binary header for `depot.lock.bin`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct DepotLockHeader {
    pub magic: [u8; 4],
    pub version: [u8; 2],
    pub flags: [u8; 2],
    pub entry_count: [u8; 4],
    pub created_unix: [u8; 8],
    pub payload_wyhash32: [u8; 4],
}

/// 94-byte aligned-1 zero-copy entry for one pinned artifact across all 10 ecosystems.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct DepotLockEntry {
    pub purl_wyhash: [u8; 8],
    pub ecosystem: u8,
    pub plane: u8,
    pub reserved: [u8; 2],
    pub purl_off: [u8; 4],
    pub purl_len: [u8; 2],
    pub ver_off: [u8; 4],
    pub ver_len: [u8; 2],
    pub abi_off: [u8; 4],
    pub abi_len: [u8; 2],
    pub sha256: [u8; 32],
    pub signer_fpr: [u8; 32],
}

const _: () = assert!(size_of::<DepotLockHeader>() == 24);
const _: () = assert!(align_of::<DepotLockHeader>() == 1);
const _: () = assert!(size_of::<DepotLockEntry>() == 94);
const _: () = assert!(align_of::<DepotLockEntry>() == 1);

/// High-level owned lock record (serialized in `depot.lock.json` and compiled to `depot.lock.bin`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRecord {
    pub purl: String,
    pub ecosystem: Ecosystem,
    pub plane: ActionPlane,
    pub version: String,
    pub abi_tag: String,
    pub sha256: [u8; 32],
    pub signer_fpr: [u8; 32],
}

fn push_str(slab: &mut Vec<u8>, s: &str) -> Result<(u32, u16), DepotError> {
    let off =
        u32::try_from(slab.len()).map_err(|_| DepotError::InvalidLock("string slab > 4 GiB"))?;
    let len =
        u16::try_from(s.len()).map_err(|_| DepotError::InvalidLock("string field > 64 KiB"))?;
    slab.extend_from_slice(s.as_bytes());
    Ok((off, len))
}

fn read_str(slab: &[u8], off_bytes: [u8; 4], len_bytes: [u8; 2]) -> Result<String, DepotError> {
    let off = u32::from_le_bytes(off_bytes) as usize;
    let len = u16::from_le_bytes(len_bytes) as usize;
    let end = off
        .checked_add(len)
        .ok_or(DepotError::InvalidLock("string offset overflow"))?;
    let slice = slab
        .get(off..end)
        .ok_or(DepotError::InvalidLock("string slice out of bounds"))?;
    std::str::from_utf8(slice)
        .map(str::to_owned)
        .map_err(|_| DepotError::InvalidLock("non-UTF-8 string slab"))
}

/// Encode lock records into a verified zero-copy `AYDL` binary lock buffer.
pub fn encode_depot_lock(
    records: &[LockRecord],
    created_unix: u64,
    flags: u16,
) -> Result<Vec<u8>, DepotError> {
    let entry_count =
        u32::try_from(records.len()).map_err(|_| DepotError::InvalidLock("too many entries"))?;
    let mut entries = Vec::with_capacity(records.len());
    let mut slab = Vec::new();

    for rec in records {
        let (purl_off, purl_len) = push_str(&mut slab, &rec.purl)?;
        let (ver_off, ver_len) = push_str(&mut slab, &rec.version)?;
        let (abi_off, abi_len) = push_str(&mut slab, &rec.abi_tag)?;
        entries.push(DepotLockEntry {
            purl_wyhash: bun_wyhash::hash(rec.purl.as_bytes()).to_le_bytes(),
            ecosystem: rec.ecosystem as u8,
            plane: rec.plane as u8,
            reserved: [0; 2],
            purl_off: purl_off.to_le_bytes(),
            purl_len: purl_len.to_le_bytes(),
            ver_off: ver_off.to_le_bytes(),
            ver_len: ver_len.to_le_bytes(),
            abi_off: abi_off.to_le_bytes(),
            abi_len: abi_len.to_le_bytes(),
            sha256: rec.sha256,
            signer_fpr: rec.signer_fpr,
        });
    }

    let mut payload = Vec::with_capacity(entries.len() * size_of::<DepotLockEntry>() + slab.len());
    payload.extend_from_slice(entries.as_slice().as_bytes());
    payload.extend_from_slice(&slab);
    let payload_hash = bun_wyhash::hash(&payload) as u32;

    let header = DepotLockHeader {
        magic: DEPOT_LOCK_MAGIC,
        version: DEPOT_LOCK_VERSION.to_le_bytes(),
        flags: flags.to_le_bytes(),
        entry_count: entry_count.to_le_bytes(),
        created_unix: created_unix.to_le_bytes(),
        payload_wyhash32: payload_hash.to_le_bytes(),
    };

    let mut out = Vec::with_capacity(size_of::<DepotLockHeader>() + payload.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Zero-copy decode & tamper-verify an `AYDL` binary lock buffer.
pub fn decode_depot_lock(bytes: &[u8]) -> Result<(DepotLockHeader, Vec<LockRecord>), DepotError> {
    let (header, rest) = DepotLockHeader::ref_from_prefix(bytes)
        .map_err(|_| DepotError::InvalidLock("truncated header"))?;
    if header.magic != DEPOT_LOCK_MAGIC {
        return Err(DepotError::InvalidLock("bad magic (expected AYDL)"));
    }
    if u16::from_le_bytes(header.version) != DEPOT_LOCK_VERSION {
        return Err(DepotError::InvalidLock("unsupported version"));
    }

    let expected = u32::from_le_bytes(header.payload_wyhash32);
    let actual = bun_wyhash::hash(rest) as u32;
    if expected != actual {
        return Err(DepotError::LockTampered { expected, actual });
    }

    let count = u32::from_le_bytes(header.entry_count) as usize;
    let table_bytes = count
        .checked_mul(size_of::<DepotLockEntry>())
        .ok_or(DepotError::InvalidLock("entry table size overflow"))?;
    let (raw_entries, slab) = rest
        .split_at_checked(table_bytes)
        .ok_or(DepotError::InvalidLock("truncated entry table"))?;
    let entries = <[DepotLockEntry]>::ref_from_bytes(raw_entries)
        .map_err(|_| DepotError::InvalidLock("entry table alignment/length mismatch"))?;

    let mut out = Vec::with_capacity(count);
    for entry in entries {
        let ecosystem = Ecosystem::from_u8(entry.ecosystem)
            .ok_or(DepotError::InvalidLock("invalid ecosystem tag"))?;
        let plane = ActionPlane::from_u8(entry.plane)
            .ok_or(DepotError::InvalidLock("invalid action plane tag"))?;
        let purl = read_str(slab, entry.purl_off, entry.purl_len)?;
        if u64::from_le_bytes(entry.purl_wyhash) != bun_wyhash::hash(purl.as_bytes()) {
            return Err(DepotError::InvalidLock("purl wyhash mismatch"));
        }
        out.push(LockRecord {
            purl,
            ecosystem,
            plane,
            version: read_str(slab, entry.ver_off, entry.ver_len)?,
            abi_tag: read_str(slab, entry.abi_off, entry.abi_len)?,
            sha256: entry.sha256,
            signer_fpr: entry.signer_fpr,
        });
    }

    Ok((*header, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zerocopy_depot_lock_roundtrips_and_rejects_tampering() {
        let records = vec![
            LockRecord {
                purl: "pkg:alpm/cachyos/ripgrep@14.1.1-1?arch=x86_64_v3".to_owned(),
                ecosystem: Ecosystem::Pacman,
                plane: ActionPlane::Consume,
                version: "14.1.1-1".to_owned(),
                abi_tag: "x86-64-v3".to_owned(),
                sha256: [0xAB; 32],
                signer_fpr: [0x11; 32],
            },
            LockRecord {
                purl: "pkg:play/com.aphrody.app@37001".to_owned(),
                ecosystem: Ecosystem::GooglePlay,
                plane: ActionPlane::Publish,
                version: "37001".to_owned(),
                abi_tag: "arm64-v8a".to_owned(),
                sha256: [0xCD; 32],
                signer_fpr: [0x22; 32],
            },
        ];

        let mut bin = encode_depot_lock(&records, 1_795_856_000, 1).expect("encode lock");
        let (hdr, decoded) = decode_depot_lock(&bin).expect("decode lock");
        assert_eq!(u32::from_le_bytes(hdr.entry_count), 2);
        assert_eq!(decoded, records);

        // Flip 1 byte in payload -> must fail tamper check
        let last = bin.len() - 1;
        bin[last] ^= 0xFF;
        assert!(matches!(
            decode_depot_lock(&bin),
            Err(DepotError::LockTampered { .. })
        ));
    }
}
