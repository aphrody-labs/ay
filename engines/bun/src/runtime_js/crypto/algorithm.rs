//! The digests of `Bun.CryptoHasher`, `node:crypto` and `crypto.subtle`: src/runtime/crypto/EVP.rs's table of names,
//! on RustCrypto instead of BoringSSL's `EVP_MD`s.

use digest::{DynDigest, ExtendableOutput, Update};
use hmac::{Mac, SimpleHmac};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Algorithm {
    Blake2b256,
    Blake2b512,
    Blake2s256,
    Md4,
    Md5,
    Ripemd160,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
    Sha512_224,
    Sha512_256,
    Sha3_224,
    Sha3_256,
    Sha3_384,
    Sha3_512,
    Shake128,
    Shake256,
}

/// `CryptoHasher.algorithms`, in EVP.rs's order.
pub const ALL: [Algorithm; 19] = [
    Algorithm::Blake2b256,
    Algorithm::Blake2b512,
    Algorithm::Blake2s256,
    Algorithm::Md4,
    Algorithm::Md5,
    Algorithm::Ripemd160,
    Algorithm::Sha1,
    Algorithm::Sha224,
    Algorithm::Sha256,
    Algorithm::Sha384,
    Algorithm::Sha512,
    Algorithm::Sha512_224,
    Algorithm::Sha512_256,
    Algorithm::Sha3_224,
    Algorithm::Sha3_256,
    Algorithm::Sha3_384,
    Algorithm::Sha3_512,
    Algorithm::Shake128,
    Algorithm::Shake256,
];

/// EVP.rs's `ALGORITHM_MAP`: lowercase name → algorithm.
const NAMES: &[(&str, Algorithm)] = &[
    ("md4", Algorithm::Md4),
    ("md5", Algorithm::Md5),
    ("sha1", Algorithm::Sha1),
    ("sha-1", Algorithm::Sha1),
    ("sha128", Algorithm::Sha1),
    ("sha224", Algorithm::Sha224),
    ("sha256", Algorithm::Sha256),
    ("sha384", Algorithm::Sha384),
    ("sha512", Algorithm::Sha512),
    ("rmd160", Algorithm::Ripemd160),
    ("sha-224", Algorithm::Sha224),
    ("sha-256", Algorithm::Sha256),
    ("sha-384", Algorithm::Sha384),
    ("sha-512", Algorithm::Sha512),
    ("sha3-224", Algorithm::Sha3_224),
    ("sha3-256", Algorithm::Sha3_256),
    ("sha3-384", Algorithm::Sha3_384),
    ("sha3-512", Algorithm::Sha3_512),
    ("shake128", Algorithm::Shake128),
    ("shake256", Algorithm::Shake256),
    ("shake-128", Algorithm::Shake128),
    ("shake-256", Algorithm::Shake256),
    ("ripemd160", Algorithm::Ripemd160),
    ("blake2b256", Algorithm::Blake2b256),
    ("blake2b512", Algorithm::Blake2b512),
    ("blake2s256", Algorithm::Blake2s256),
    ("sha-512224", Algorithm::Sha512_224),
    ("sha512-224", Algorithm::Sha512_224),
    ("sha-512256", Algorithm::Sha512_256),
    ("sha512-256", Algorithm::Sha512_256),
    ("blake2b-512", Algorithm::Blake2b512),
    ("blake2s-256", Algorithm::Blake2s256),
    ("sha-512/224", Algorithm::Sha512_224),
    ("sha-512_224", Algorithm::Sha512_224),
    ("sha-512/256", Algorithm::Sha512_256),
    ("sha-512_256", Algorithm::Sha512_256),
];

type Blake2b256 = blake2::Blake2b<digest::consts::U32>;

impl Algorithm {
    /// `lookup_ignore_case`.
    pub fn lookup(name: &str) -> Option<Algorithm> {
        NAMES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .map(|&(_, algorithm)| algorithm)
    }

    /// `tag_cstr`.
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Blake2b256 => "blake2b256",
            Algorithm::Blake2b512 => "blake2b512",
            Algorithm::Blake2s256 => "blake2s256",
            Algorithm::Md4 => "md4",
            Algorithm::Md5 => "md5",
            Algorithm::Ripemd160 => "ripemd160",
            Algorithm::Sha1 => "sha1",
            Algorithm::Sha224 => "sha224",
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha384 => "sha384",
            Algorithm::Sha512 => "sha512",
            Algorithm::Sha512_224 => "sha512-224",
            Algorithm::Sha512_256 => "sha512-256",
            Algorithm::Sha3_224 => "sha3-224",
            Algorithm::Sha3_256 => "sha3-256",
            Algorithm::Sha3_384 => "sha3-384",
            Algorithm::Sha3_512 => "sha3-512",
            Algorithm::Shake128 => "shake128",
            Algorithm::Shake256 => "shake256",
        }
    }

    pub fn is_xof(self) -> bool {
        matches!(self, Algorithm::Shake128 | Algorithm::Shake256)
    }
}

/// A hash being computed: a fixed-size digest, or SHAKE with its output length (OpenSSL's defaults: 16 and 32).
pub enum Hasher {
    Fixed(Box<dyn DynDigest>),
    Shake128(sha3::Shake128, usize),
    Shake256(sha3::Shake256, usize),
}

impl Clone for Hasher {
    fn clone(&self) -> Self {
        match self {
            Hasher::Fixed(digest) => Hasher::Fixed(digest.box_clone()),
            Hasher::Shake128(xof, len) => Hasher::Shake128(xof.clone(), *len),
            Hasher::Shake256(xof, len) => Hasher::Shake256(xof.clone(), *len),
        }
    }
}

impl Hasher {
    /// `output_length` is SHAKE's; the others have theirs.
    pub fn new(algorithm: Algorithm, output_length: Option<usize>) -> Hasher {
        let fixed: Box<dyn DynDigest> = match algorithm {
            Algorithm::Blake2b256 => Box::new(Blake2b256::default()),
            Algorithm::Blake2b512 => Box::new(blake2::Blake2b512::default()),
            Algorithm::Blake2s256 => Box::new(blake2::Blake2s256::default()),
            Algorithm::Md4 => Box::new(md4::Md4::default()),
            Algorithm::Md5 => Box::new(md5::Md5::default()),
            Algorithm::Ripemd160 => Box::new(ripemd::Ripemd160::default()),
            Algorithm::Sha1 => Box::new(sha1::Sha1::default()),
            Algorithm::Sha224 => Box::new(sha2::Sha224::default()),
            Algorithm::Sha256 => Box::new(sha2::Sha256::default()),
            Algorithm::Sha384 => Box::new(sha2::Sha384::default()),
            Algorithm::Sha512 => Box::new(sha2::Sha512::default()),
            Algorithm::Sha512_224 => Box::new(sha2::Sha512_224::default()),
            Algorithm::Sha512_256 => Box::new(sha2::Sha512_256::default()),
            Algorithm::Sha3_224 => Box::new(sha3::Sha3_224::default()),
            Algorithm::Sha3_256 => Box::new(sha3::Sha3_256::default()),
            Algorithm::Sha3_384 => Box::new(sha3::Sha3_384::default()),
            Algorithm::Sha3_512 => Box::new(sha3::Sha3_512::default()),
            Algorithm::Shake128 => {
                return Hasher::Shake128(Default::default(), output_length.unwrap_or(16));
            }
            Algorithm::Shake256 => {
                return Hasher::Shake256(Default::default(), output_length.unwrap_or(32));
            }
        };
        Hasher::Fixed(fixed)
    }

    pub fn output_length(&self) -> usize {
        match self {
            Hasher::Fixed(digest) => digest.output_size(),
            Hasher::Shake128(_, len) | Hasher::Shake256(_, len) => *len,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match self {
            Hasher::Fixed(digest) => digest.update(bytes),
            Hasher::Shake128(xof, _) => Update::update(xof, bytes),
            Hasher::Shake256(xof, _) => Update::update(xof, bytes),
        }
    }

    /// `xof_length` replaces SHAKE's output length.
    pub fn finish_with(self, xof_length: Option<usize>) -> Vec<u8> {
        match self {
            Hasher::Shake128(xof, len) => Hasher::Shake128(xof, xof_length.unwrap_or(len)).finish(),
            Hasher::Shake256(xof, len) => Hasher::Shake256(xof, xof_length.unwrap_or(len)).finish(),
            fixed => fixed.finish(),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        match self {
            Hasher::Fixed(digest) => digest.finalize().into_vec(),
            Hasher::Shake128(xof, len) => {
                let mut out = vec![0; len];
                xof.finalize_xof_into(&mut out);
                out
            }
            Hasher::Shake256(xof, len) => {
                let mut out = vec![0; len];
                xof.finalize_xof_into(&mut out);
                out
            }
        }
    }
}

/// An HMAC of any of the fixed-size digests.
pub trait DynMac {
    fn update(&mut self, bytes: &[u8]);
    fn finish(self: Box<Self>) -> Vec<u8>;
    fn box_clone(&self) -> Box<dyn DynMac>;
    fn output_length(&self) -> usize;
}

impl<M: Mac + Clone + 'static> DynMac for M {
    fn update(&mut self, bytes: &[u8]) {
        Mac::update(self, bytes);
    }
    fn finish(self: Box<Self>) -> Vec<u8> {
        self.finalize().into_bytes().to_vec()
    }
    fn box_clone(&self) -> Box<dyn DynMac> {
        Box::new(self.clone())
    }
    fn output_length(&self) -> usize {
        <M as digest::OutputSizeUser>::output_size()
    }
}

/// HMAC with `algorithm` keyed by `key`: `None` for SHAKE, which has no HMAC.
pub fn hmac(algorithm: Algorithm, key: &[u8]) -> Option<Box<dyn DynMac>> {
    fn keyed<D>(key: &[u8]) -> Box<dyn DynMac>
    where
        D: digest::Digest + digest::core_api::BlockSizeUser + Clone + 'static,
    {
        Box::new(SimpleHmac::<D>::new_from_slice(key).expect("HMAC takes a key of any length"))
    }
    Some(match algorithm {
        Algorithm::Blake2b256 => keyed::<Blake2b256>(key),
        Algorithm::Blake2b512 => keyed::<blake2::Blake2b512>(key),
        Algorithm::Blake2s256 => keyed::<blake2::Blake2s256>(key),
        Algorithm::Md4 => keyed::<md4::Md4>(key),
        Algorithm::Md5 => keyed::<md5::Md5>(key),
        Algorithm::Ripemd160 => keyed::<ripemd::Ripemd160>(key),
        Algorithm::Sha1 => keyed::<sha1::Sha1>(key),
        Algorithm::Sha224 => keyed::<sha2::Sha224>(key),
        Algorithm::Sha256 => keyed::<sha2::Sha256>(key),
        Algorithm::Sha384 => keyed::<sha2::Sha384>(key),
        Algorithm::Sha512 => keyed::<sha2::Sha512>(key),
        Algorithm::Sha512_224 => keyed::<sha2::Sha512_224>(key),
        Algorithm::Sha512_256 => keyed::<sha2::Sha512_256>(key),
        Algorithm::Sha3_224 => keyed::<sha3::Sha3_224>(key),
        Algorithm::Sha3_256 => keyed::<sha3::Sha3_256>(key),
        Algorithm::Sha3_384 => keyed::<sha3::Sha3_384>(key),
        Algorithm::Sha3_512 => keyed::<sha3::Sha3_512>(key),
        Algorithm::Shake128 | Algorithm::Shake256 => return None,
    })
}

/// PBKDF2-HMAC with `algorithm` (PBKDF2.rs): `None` for SHAKE.
pub fn pbkdf2(
    algorithm: Algorithm,
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    out: &mut [u8],
) -> Option<()> {
    fn run<D>(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8])
    where
        D: digest::Digest + digest::core_api::BlockSizeUser + Clone + Sync + 'static,
    {
        pbkdf2::pbkdf2::<SimpleHmac<D>>(password, salt, iterations, out)
            .expect("HMAC takes a key of any length");
    }
    match algorithm {
        Algorithm::Blake2b256 => run::<Blake2b256>(password, salt, iterations, out),
        Algorithm::Blake2b512 => run::<blake2::Blake2b512>(password, salt, iterations, out),
        Algorithm::Blake2s256 => run::<blake2::Blake2s256>(password, salt, iterations, out),
        Algorithm::Md4 => run::<md4::Md4>(password, salt, iterations, out),
        Algorithm::Md5 => run::<md5::Md5>(password, salt, iterations, out),
        Algorithm::Ripemd160 => run::<ripemd::Ripemd160>(password, salt, iterations, out),
        Algorithm::Sha1 => run::<sha1::Sha1>(password, salt, iterations, out),
        Algorithm::Sha224 => run::<sha2::Sha224>(password, salt, iterations, out),
        Algorithm::Sha256 => run::<sha2::Sha256>(password, salt, iterations, out),
        Algorithm::Sha384 => run::<sha2::Sha384>(password, salt, iterations, out),
        Algorithm::Sha512 => run::<sha2::Sha512>(password, salt, iterations, out),
        Algorithm::Sha512_224 => run::<sha2::Sha512_224>(password, salt, iterations, out),
        Algorithm::Sha512_256 => run::<sha2::Sha512_256>(password, salt, iterations, out),
        Algorithm::Sha3_224 => run::<sha3::Sha3_224>(password, salt, iterations, out),
        Algorithm::Sha3_256 => run::<sha3::Sha3_256>(password, salt, iterations, out),
        Algorithm::Sha3_384 => run::<sha3::Sha3_384>(password, salt, iterations, out),
        Algorithm::Sha3_512 => run::<sha3::Sha3_512>(password, salt, iterations, out),
        Algorithm::Shake128 | Algorithm::Shake256 => return None,
    }
    Some(())
}

/// HKDF (RFC 5869) with `algorithm`'s HMAC: `None` for SHAKE, or for more than 255 blocks of output.
pub fn hkdf(
    algorithm: Algorithm,
    ikm: &[u8],
    salt: &[u8],
    info: &[u8],
    length: usize,
) -> Option<Vec<u8>> {
    let size = hmac(algorithm, &[])?.output_length();
    if length > 255 * size {
        return None;
    }
    let salt = if salt.is_empty() {
        vec![0; size]
    } else {
        salt.to_vec()
    };
    let mut extract = hmac(algorithm, &salt)?;
    extract.update(ikm);
    let prk = extract.finish();
    let mut out = Vec::with_capacity(length);
    let mut previous = Vec::new();
    let mut counter = 1u8;
    while out.len() < length {
        let mut expand = hmac(algorithm, &prk)?;
        expand.update(&previous);
        expand.update(info);
        expand.update(&[counter]);
        previous = expand.finish();
        out.extend_from_slice(&previous);
        counter = counter.wrapping_add(1);
    }
    out.truncate(length);
    Some(out)
}
