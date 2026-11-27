//! src/jsc/uuid.rs on getrandom and RustCrypto's SHA-1: the UUIDs of `crypto.randomUUID()`, `Bun.randomUUIDv7()`
//! and `Bun.randomUUIDv5()`.

use std::sync::Mutex;

use sha1::Digest as _;

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).expect("the OS gives random bytes");
    bytes
}

/// Version 4: random.
pub fn v4() -> [u8; 16] {
    let mut bytes = random::<16>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes
}

/// Where the timestamp of a v7 came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TimestampSource {
    /// The clock: the RFC 9562 §6.2 monotonic clamp applies.
    Clock,
    /// The caller's, encoded as it is, without touching the clock's state.
    Explicit,
}

struct V7State {
    last_timestamp: u64,
    counter: u32,
    explicit_requested: u64,
    explicit_emitted: u64,
    explicit_counter: u32,
}

static V7: Mutex<V7State> = Mutex::new(V7State {
    last_timestamp: 0,
    counter: 0,
    explicit_requested: u64::MAX,
    explicit_emitted: 0,
    explicit_counter: 0,
});

/// The timestamp to encode and the 12-bit `rand_a` counter: on rollover the timestamp moves on instead.
fn next(timestamp: u64, seed: u16, source: TimestampSource) -> (u64, u32) {
    let seed = u32::from(seed & 0x07FF);
    let mut state = V7.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut ts, mut count) = match source {
        TimestampSource::Clock if timestamp > state.last_timestamp => (timestamp, seed),
        TimestampSource::Clock => (state.last_timestamp, state.counter + 1),
        TimestampSource::Explicit if timestamp != state.explicit_requested => (timestamp, seed),
        TimestampSource::Explicit => (state.explicit_emitted, state.explicit_counter + 1),
    };
    if count > 0x0FFF {
        ts = (ts + 1).min((1u64 << 48) - 1);
        count = seed;
    }
    match source {
        TimestampSource::Clock => {
            state.last_timestamp = ts;
            state.counter = count;
        }
        TimestampSource::Explicit => {
            state.explicit_requested = timestamp;
            state.explicit_emitted = ts;
            state.explicit_counter = count;
        }
    }
    (ts, count)
}

/// Version 7: `unix_ts_ms | version | rand_a (a counter) | variant | rand_b`.
pub fn v7(timestamp: u64, source: TimestampSource) -> [u8; 16] {
    let random = random::<10>();
    let seed = u16::from_le_bytes([random[8], random[9]]);
    let (timestamp, count) = next(timestamp, seed, source);
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&timestamp.to_be_bytes()[2..]);
    bytes[6] = (7u8 << 4) | ((count >> 8) & 0x0F) as u8;
    bytes[7] = count as u8;
    bytes[8] = 0x80 | (random[0] & 0x3F);
    bytes[9..16].copy_from_slice(&random[1..8]);
    bytes
}

/// Version 5: SHA-1 of the namespace and the name.
pub fn v5(namespace: &[u8; 16], name: &[u8]) -> [u8; 16] {
    let mut hasher = sha1::Sha1::new();
    hasher.update(namespace);
    hasher.update(name);
    let hash = hasher.finalize();
    let mut bytes: [u8; 16] = hash[..16].try_into().expect("SHA-1 has 20 bytes");
    bytes[6] = (bytes[6] & 0x0F) | 0x50;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    bytes
}

/// The namespaces of RFC 4122 Appendix C, by name.
pub fn namespace(name: &str) -> Option<[u8; 16]> {
    let last = match name.to_ascii_lowercase().as_str() {
        "dns" => 0x10,
        "url" => 0x11,
        "oid" => 0x12,
        "x500" => 0x14,
        _ => return None,
    };
    Some([
        0x6b, 0xa7, 0xb8, last, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ])
}

/// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, lowercase.
pub fn format(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `UUID::parse`: the 16 bytes of a UUID string, `None` for anything else.
pub fn parse(text: &str) -> Option<[u8; 16]> {
    let text = text.as_bytes();
    if text.len() != 36
        || text[8] != b'-'
        || text[13] != b'-'
        || text[18] != b'-'
        || text[23] != b'-'
    {
        return None;
    }
    const POSITIONS: [usize; 16] = [0, 2, 4, 6, 9, 11, 14, 16, 19, 21, 24, 26, 28, 30, 32, 34];
    let mut bytes = [0u8; 16];
    for (byte, &at) in bytes.iter_mut().zip(POSITIONS.iter()) {
        let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        *byte = (digit(text[at])? << 4) | digit(text[at + 1])?;
    }
    Some(bytes)
}
