//! The BoringSSL digests that `bun_sha_hmac` and the package manager call (tarball integrity is SHA-1 or SHA-512,
//! the cache keys SHA-256), on RustCrypto, and `RAND_bytes` on the OS. The `EVP_MD_CTX` is BoringSSL's layout,
//! which callers keep by value: its `md_data` holds a `Box` of the hasher, which `EVP_MD_CTX_cleanup` and
//! `EVP_DigestFinal` free.

use core::ffi::{c_int, c_uint, c_void};

use bun_boringssl_sys::{ENGINE, EVP_MD, EVP_MD_CTX, SHA_CTX};
use sha2::Digest as _;

enum State {
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
    Sha512_256(sha2::Sha512_256),
}

/// The `Box<State>` in `md_data`, null when there is none.
fn slot(ctx: *mut EVP_MD_CTX) -> *mut *mut State {
    // SAFETY: only the address of the field, which has room and alignment for a pointer.
    unsafe { (&raw mut (*ctx).md_data).cast() }
}

static SHA1: u8 = 1;
static SHA256: u8 = 2;
static SHA384: u8 = 3;
static SHA512: u8 = 4;
static SHA512_256: u8 = 5;

fn marker(m: &'static u8) -> *const EVP_MD {
    core::ptr::from_ref(m).cast()
}

#[unsafe(no_mangle)]
extern "C" fn EVP_sha1() -> *const EVP_MD {
    marker(&SHA1)
}
#[unsafe(no_mangle)]
extern "C" fn EVP_sha256() -> *const EVP_MD {
    marker(&SHA256)
}
#[unsafe(no_mangle)]
extern "C" fn EVP_sha384() -> *const EVP_MD {
    marker(&SHA384)
}
#[unsafe(no_mangle)]
extern "C" fn EVP_sha512() -> *const EVP_MD {
    marker(&SHA512)
}
#[unsafe(no_mangle)]
extern "C" fn EVP_sha512_256() -> *const EVP_MD {
    marker(&SHA512_256)
}

fn new_state(md: *const EVP_MD) -> State {
    match md {
        md if md == EVP_sha1() => State::Sha1(sha1::Sha1::new()),
        md if md == EVP_sha256() => State::Sha256(sha2::Sha256::new()),
        md if md == EVP_sha384() => State::Sha384(sha2::Sha384::new()),
        md if md == EVP_sha512() => State::Sha512(sha2::Sha512::new()),
        md if md == EVP_sha512_256() => State::Sha512_256(sha2::Sha512_256::new()),
        _ => panic!("this build has SHA-1, SHA-256, SHA-384, SHA-512 and SHA-512/256 digests only"),
    }
}

#[unsafe(no_mangle)]
extern "C" fn EVP_MD_size(md: *const EVP_MD) -> usize {
    match md {
        md if md == EVP_sha1() => 20,
        md if md == EVP_sha384() => 48,
        md if md == EVP_sha512() => 64,
        _ => 32,
    }
}

/// # Safety
/// `ctx` is a context that `EVP_DigestInit` initialized.
unsafe fn state<'a>(ctx: *mut EVP_MD_CTX) -> &'a mut State {
    // SAFETY: the contract of this function.
    unsafe {
        let state = *slot(ctx);
        assert!(!(*ctx).digest.is_null() && !state.is_null(), "EVP_DigestInit has not run");
        &mut *state
    }
}

/// # Safety
/// `ctx` is a context, zeroed or initialized.
unsafe fn free_state(ctx: *mut EVP_MD_CTX) {
    // SAFETY: the contract of this function: the slot is null or a box of ours.
    unsafe {
        let state = *slot(ctx);
        if !(*ctx).digest.is_null() && !state.is_null() {
            drop(Box::from_raw(state));
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn EVP_MD_CTX_init(ctx: *mut EVP_MD_CTX) {
    // SAFETY: the caller's room for a context; all-zero is the empty one.
    unsafe { ctx.write(bun_core::ffi::zeroed()) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn EVP_MD_CTX_cleanup(ctx: *mut EVP_MD_CTX) -> c_int {
    // SAFETY: the caller's context.
    unsafe {
        free_state(ctx);
        ctx.write(bun_core::ffi::zeroed());
    }
    1
}

#[unsafe(no_mangle)]
unsafe extern "C" fn EVP_DigestInit_ex(ctx: *mut EVP_MD_CTX, md: *const EVP_MD, _engine: *mut ENGINE) -> c_int {
    // SAFETY: the caller's room for a context.
    unsafe {
        ctx.write(EVP_MD_CTX { digest: md, ..bun_core::ffi::zeroed() });
        *slot(ctx) = Box::into_raw(Box::new(new_state(md)));
    }
    1
}

#[unsafe(no_mangle)]
unsafe extern "C" fn EVP_DigestInit(ctx: *mut EVP_MD_CTX, md: *const EVP_MD) -> c_int {
    // SAFETY: the caller's room for a context.
    unsafe { EVP_DigestInit_ex(ctx, md, core::ptr::null_mut()) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn EVP_DigestUpdate(ctx: *mut EVP_MD_CTX, data: *const c_void, len: usize) -> c_int {
    // SAFETY: an initialized context, and the caller's slice.
    unsafe {
        let data = if len == 0 { &[][..] } else { core::slice::from_raw_parts(data.cast::<u8>(), len) };
        match state(ctx) {
            State::Sha1(h) => h.update(data),
            State::Sha256(h) => h.update(data),
            State::Sha384(h) => h.update(data),
            State::Sha512(h) => h.update(data),
            State::Sha512_256(h) => h.update(data),
        }
    }
    1
}

#[unsafe(no_mangle)]
unsafe extern "C" fn EVP_DigestFinal_ex(ctx: *mut EVP_MD_CTX, out: *mut u8, out_size: *mut c_uint) -> c_int {
    // SAFETY: an initialized context, and room for the digest at `out`.
    unsafe {
        state(ctx);
        let boxed = Box::from_raw(*slot(ctx));
        *slot(ctx) = core::ptr::null_mut();
        let digest: Vec<u8> = match *boxed {
            State::Sha1(h) => h.finalize().to_vec(),
            State::Sha256(h) => h.finalize().to_vec(),
            State::Sha384(h) => h.finalize().to_vec(),
            State::Sha512(h) => h.finalize().to_vec(),
            State::Sha512_256(h) => h.finalize().to_vec(),
        };
        core::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len());
        if !out_size.is_null() {
            *out_size = digest.len() as c_uint;
        }
    }
    1
}

#[unsafe(no_mangle)]
unsafe extern "C" fn EVP_DigestFinal(ctx: *mut EVP_MD_CTX, out: *mut u8, out_size: *mut c_uint) -> c_int {
    // SAFETY: as above.
    unsafe {
        EVP_DigestFinal_ex(ctx, out, out_size);
        EVP_MD_CTX_cleanup(ctx)
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn EVP_Digest(
    data: *const c_void,
    len: usize,
    out: *mut u8,
    out_size: *mut c_uint,
    md: *const EVP_MD,
    _engine: *mut ENGINE,
) -> c_int {
    let mut ctx: EVP_MD_CTX = bun_core::ffi::zeroed();
    // SAFETY: a local context, and the caller's pointers.
    unsafe {
        EVP_DigestInit(&mut ctx, md);
        EVP_DigestUpdate(&mut ctx, data, len);
        EVP_DigestFinal(&mut ctx, out, out_size)
    }
}

// ── SHA-1 on BoringSSL's SHA_CTX ───────────────────────────────────────────

fn sha1_compress(h: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    for t in 16..80 {
        w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *h;
    for (t, word) in w.iter().enumerate() {
        let (f, k) = match t {
            0..20 => ((b & c) | (!b & d), 0x5A82_7999),
            20..40 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..60 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }
    for (state, value) in h.iter_mut().zip([a, b, c, d, e]) {
        *state = state.wrapping_add(value);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn SHA1_Init(sha: *mut SHA_CTX) -> c_int {
    // SAFETY: the caller's room for a context.
    unsafe {
        sha.write(SHA_CTX {
            h: [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0],
            Nl: 0,
            Nh: 0,
            data: [0; 64],
            num: 0,
        });
    }
    1
}

#[unsafe(no_mangle)]
unsafe extern "C" fn SHA1_Update(sha: *mut SHA_CTX, data: *const c_void, len: usize) -> c_int {
    // SAFETY: a context that SHA1_Init wrote, and the caller's slice.
    unsafe {
        let sha = &mut *sha;
        let mut input = if len == 0 { &[][..] } else { core::slice::from_raw_parts(data.cast::<u8>(), len) };
        let bits = (u64::from(sha.Nh) << 32 | u64::from(sha.Nl)).wrapping_add((len as u64) << 3);
        (sha.Nl, sha.Nh) = (bits as u32, (bits >> 32) as u32);
        let mut num = sha.num as usize;
        while !input.is_empty() {
            let take = (64 - num).min(input.len());
            sha.data[num..num + take].copy_from_slice(&input[..take]);
            num += take;
            input = &input[take..];
            if num == 64 {
                let block = sha.data;
                sha1_compress(&mut sha.h, &block);
                num = 0;
            }
        }
        sha.num = num as c_uint;
    }
    1
}

#[unsafe(no_mangle)]
unsafe extern "C" fn SHA1_Final(out: *mut u8, sha: *mut SHA_CTX) -> c_int {
    // SAFETY: a context that SHA1_Init wrote, and room for 20 bytes.
    unsafe {
        let sha = &mut *sha;
        let bits = u64::from(sha.Nh) << 32 | u64::from(sha.Nl);
        let mut num = sha.num as usize;
        sha.data[num] = 0x80;
        num += 1;
        if num > 56 {
            sha.data[num..].fill(0);
            let block = sha.data;
            sha1_compress(&mut sha.h, &block);
            num = 0;
        }
        sha.data[num..56].fill(0);
        sha.data[56..].copy_from_slice(&bits.to_be_bytes());
        let block = sha.data;
        sha1_compress(&mut sha.h, &block);
        for (i, word) in sha.h.iter().enumerate() {
            core::ptr::copy_nonoverlapping(word.to_be_bytes().as_ptr(), out.add(i * 4), 4);
        }
    }
    1
}

#[unsafe(no_mangle)]
unsafe extern "C" fn RAND_bytes(buf: *mut u8, len: usize) -> c_int {
    if len == 0 {
        return 1;
    }
    // SAFETY: the caller's buffer of `len` bytes.
    let buf = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    c_int::from(getrandom::fill(buf).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn evp(md: *const EVP_MD, data: &[u8]) -> String {
        let mut out = [0u8; 64];
        let mut size: c_uint = 0;
        // SAFETY: locals.
        unsafe { EVP_Digest(data.as_ptr().cast(), data.len(), out.as_mut_ptr(), &mut size, md, core::ptr::null_mut()) };
        hex(&out[..size as usize])
    }

    #[test]
    fn digests_match_their_test_vectors() {
        assert_eq!(evp(EVP_sha1(), b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(evp(EVP_sha256(), b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(&evp(EVP_sha512(), b"abc")[..16], "ddaf35a193617aba");
        assert_eq!(&evp(EVP_sha384(), b"abc")[..16], "cb00753f45a35e8b");
        assert_eq!(&evp(EVP_sha512_256(), b"abc")[..16], "53048e2681941ef9");

        let message = vec![b'a'; 1000];
        let mut ctx: SHA_CTX = bun_core::ffi::zeroed();
        let mut out = [0u8; 20];
        // SAFETY: locals.
        unsafe {
            SHA1_Init(&mut ctx);
            for chunk in message.chunks(77) {
                SHA1_Update(&mut ctx, chunk.as_ptr().cast(), chunk.len());
            }
            SHA1_Final(out.as_mut_ptr(), &mut ctx);
        }
        assert_eq!(hex(&out), evp(EVP_sha1(), &message));
    }
}
