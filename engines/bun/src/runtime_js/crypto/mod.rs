//! Crypto on `bun_js` without BoringSSL: RustCrypto and getrandom under `crypto.getRandomValues()`,
//! `crypto.randomUUID()`, `crypto.subtle`, `Bun.randomUUIDv7()`/`v5()`, `Bun.hash`, `Bun.CryptoHasher`, `Bun.SHA*`
//! and `node:crypto`'s hashes, HMACs and key derivations.
//!
//! These are the natives, on bytes: packages/bun-boa/src/js/crypto.ts is the WebIDL and argument handling of
//! src/runtime/crypto/CryptoHasher.bind.ts, src/runtime/api/HashObject.bind.ts and WebCore's `Crypto`, on them.

pub mod algorithm;
pub mod uuid;

use std::cell::RefCell;

use boa_engine::object::builtins::{JsArray, JsUint8Array};
use boa_engine::object::{JsData, ObjectInitializer};
use boa_engine::{
    Context, JsArgs, JsBigInt, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
};
use boa_gc::{Finalize, Trace};
use bun_js::{JSGlobalObject, JSValue};

use algorithm::{Algorithm, DynMac, Hasher};

fn bytes_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let global = JSGlobalObject::from_context(context);
    match JSValue::from_engine(value.clone()).array_buffer_view_bytes(global) {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(JsNativeError::typ()
            .with_message("expected a Uint8Array")
            .into()),
        Err(error) => Err(global.into_engine_error(error)),
    }
}

fn uint8_array(bytes: &[u8], context: &mut Context) -> JsResult<JsValue> {
    Ok(JsUint8Array::from_iter(bytes.iter().copied(), context)?.into())
}

fn string_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

fn usize_of(value: &JsValue, context: &mut Context) -> JsResult<usize> {
    let number = value.to_number(context)?;
    if !(0.0..=9_007_199_254_740_991.0).contains(&number) {
        return Err(JsNativeError::range()
            .with_message("length out of range")
            .into());
    }
    Ok(number as usize)
}

fn algorithm_of(value: &JsValue, context: &mut Context) -> JsResult<Option<Algorithm>> {
    Ok(Algorithm::lookup(&string_of(value, context)?))
}

/// `randomBytes(length)`.
fn random_bytes(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut bytes = vec![0; usize_of(args.get_or_undefined(0), context)?];
    getrandom::fill(&mut bytes)
        .map_err(|error| JsNativeError::error().with_message(error.to_string()))?;
    uint8_array(&bytes, context)
}

/// `uuidV4()`: the string.
fn uuid_v4(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsString::from(uuid::format(&uuid::v4())).into())
}

/// `uuidV7(timestamp | undefined)`: the 16 bytes.
fn uuid_v7(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let bytes = match args.get_or_undefined(0) {
        timestamp if timestamp.is_undefined() => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as u64);
            uuid::v7(now, uuid::TimestampSource::Clock)
        }
        timestamp => uuid::v7(
            timestamp.to_number(context)? as u64,
            uuid::TimestampSource::Explicit,
        ),
    };
    uint8_array(&bytes, context)
}

/// `uuidV5(name: Uint8Array, namespace: string | Uint8Array)`: the 16 bytes, `null` for a namespace that is not one.
fn uuid_v5(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = bytes_of(args.get_or_undefined(0), context)?;
    let namespace = match args.get_or_undefined(1) {
        namespace if namespace.is_string() => {
            let text = string_of(namespace, context)?;
            uuid::namespace(&text).or_else(|| uuid::parse(&text))
        }
        namespace => <[u8; 16]>::try_from(bytes_of(namespace, context)?).ok(),
    };
    let Some(namespace) = namespace else {
        return Ok(JsValue::null());
    };
    uint8_array(&uuid::v5(&namespace, &name), context)
}

/// `uuidString(bytes)`.
fn uuid_string(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let bytes = <[u8; 16]>::try_from(bytes_of(args.get_or_undefined(0), context)?)
        .map_err(|_| JsNativeError::typ().with_message("a UUID has 16 bytes"))?;
    Ok(JsString::from(uuid::format(&bytes)).into())
}

/// `hash(kind, data, seed)`: src/runtime/api/HashObject.rs; `seed` is the decimal digits of a u64.
fn hash(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = string_of(args.get_or_undefined(0), context)?;
    let data = bytes_of(args.get_or_undefined(1), context)?;
    let seed: u64 = string_of(args.get_or_undefined(2), context)?
        .parse()
        .unwrap_or(0);
    let small = |value: u32| Ok(JsValue::from(value));
    let big = |value: u64| Ok(JsValue::from(JsBigInt::from(value)));
    match kind.as_str() {
        "wyhash" => big(bun_wyhash::Wyhash::hash(seed, &data)),
        "adler32" => small(bun_hash::adler32(&data)),
        "crc32" => small(bun_hash::crc32(seed as u32, &data)),
        "cityHash32" => small(bun_hash::CityHash32::hash(&data)),
        "cityHash64" => big(bun_hash::CityHash64::hash_with_seed(&data, seed)),
        "xxHash32" => small(bun_hash::XxHash32::hash(seed as u32, &data)),
        "xxHash64" => big(bun_hash::XxHash64::hash(seed, &data)),
        "xxHash3" => big(bun_hash::xxhash3_64(u64::from(seed as u32), &data)),
        "murmur32v2" => small(bun_hash::Murmur2_32::hash_with_seed(&data, seed as u32)),
        "murmur32v3" => small(bun_hash::Murmur3_32::hash_with_seed(&data, seed as u32)),
        "murmur64v2" => big(bun_hash::Murmur2_64::hash_with_seed(&data, seed)),
        "rapidhash" => big(bun_hash::RapidHash::hash(seed, &data)),
        _ => Err(JsNativeError::typ()
            .with_message(format!("no hash {kind}"))
            .into()),
    }
}

/// `algorithms()`: `CryptoHasher.algorithms`.
fn algorithms(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let names = algorithm::ALL
        .iter()
        .map(|algorithm| JsString::from(algorithm.name()).into());
    Ok(JsArray::from_iter(names, context).into())
}

/// `canonicalName(name)`: the name a known algorithm goes by, or `null`.
fn canonical_name(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(
        algorithm_of(args.get_or_undefined(0), context)?.map_or(JsValue::null(), |algorithm| {
            JsString::from(algorithm.name()).into()
        }),
    )
}

enum State {
    Hash(Algorithm, Hasher),
    Hmac(Algorithm, Box<dyn DynMac>),
}

impl State {
    fn copy(&self) -> State {
        match self {
            State::Hash(algorithm, hasher) => State::Hash(*algorithm, hasher.clone()),
            State::Hmac(algorithm, mac) => State::Hmac(*algorithm, mac.box_clone()),
        }
    }
}

/// A hash or an HMAC being computed: `None` once it is digested.
#[derive(Trace, Finalize)]
struct HasherHandle(#[unsafe_ignore_trace] RefCell<Option<State>>);

impl JsData for HasherHandle {}

fn handle(state: State) -> JsValue {
    JsObject::from_proto_and_data(None, HasherHandle(RefCell::new(Some(state)))).into()
}

fn with_state<R>(
    value: &JsValue,
    f: impl FnOnce(&mut Option<State>) -> JsResult<R>,
) -> JsResult<R> {
    let object = value
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("not a hasher"))?;
    let data = object
        .downcast_ref::<HasherHandle>()
        .ok_or_else(|| JsNativeError::typ().with_message("not a hasher"))?;
    let mut state = data.0.borrow_mut();
    f(&mut state)
}

/// `hasherCreate(name, hmacKey | undefined, outputLength | undefined)`: a handle, `null` for a name that is not an
/// algorithm, `false` for an algorithm without HMAC.
fn hasher_create(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(algorithm) = algorithm_of(args.get_or_undefined(0), context)? else {
        return Ok(JsValue::null());
    };
    let key = args.get_or_undefined(1);
    if !key.is_undefined() {
        let key = bytes_of(key, context)?;
        return Ok(
            algorithm::hmac(algorithm, &key).map_or(JsValue::from(false), |mac| {
                handle(State::Hmac(algorithm, mac))
            }),
        );
    }
    let length = match args.get_or_undefined(2) {
        length if length.is_undefined() => None,
        length => Some(usize_of(length, context)?),
    };
    Ok(handle(State::Hash(
        algorithm,
        Hasher::new(algorithm, length),
    )))
}

/// `hasherUpdate(handle, bytes)`: `false` once digested.
fn hasher_update(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let bytes = bytes_of(args.get_or_undefined(1), context)?;
    with_state(args.get_or_undefined(0), |state| {
        Ok(JsValue::from(match state {
            Some(State::Hash(_, hasher)) => {
                hasher.update(&bytes);
                true
            }
            Some(State::Hmac(_, mac)) => {
                mac.update(&bytes);
                true
            }
            None => false,
        }))
    })
}

/// `hasherDigest(handle, xofLength | undefined)`: the digest, `null` once an HMAC is digested. A hash starts over,
/// as `EVP_DigestFinal` resets the context.
fn hasher_digest(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let xof_length = match args.get_or_undefined(1) {
        length if length.is_undefined() => None,
        length => Some(usize_of(length, context)?),
    };
    let digest = with_state(args.get_or_undefined(0), |state| {
        Ok(match state.take() {
            Some(State::Hash(algorithm, hasher)) => {
                *state = Some(State::Hash(
                    algorithm,
                    Hasher::new(algorithm, Some(hasher.output_length())),
                ));
                Some(hasher.finish_with(xof_length))
            }
            Some(State::Hmac(_, mac)) => Some(mac.finish()),
            None => None,
        })
    })?;
    match digest {
        Some(digest) => uint8_array(&digest, context),
        None => Ok(JsValue::null()),
    }
}

/// `hasherCopy(handle)`: a handle of the same state, `null` once digested.
fn hasher_copy(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    with_state(args.get_or_undefined(0), |state| {
        Ok(state.as_ref().map_or(JsValue::null(), |s| handle(s.copy())))
    })
}

/// `hasherInfo(handle)`: `[algorithm, byteLength, isHmac]`, `null` once digested.
fn hasher_info(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let info = with_state(args.get_or_undefined(0), |state| {
        Ok(state.as_ref().map(|state| match state {
            State::Hash(algorithm, hasher) => (algorithm.name(), hasher.output_length(), false),
            State::Hmac(algorithm, mac) => (algorithm.name(), mac.output_length(), true),
        }))
    })?;
    Ok(match info {
        Some((name, length, hmac)) => JsArray::from_iter(
            [
                JsString::from(name).into(),
                JsValue::from(length as u32),
                JsValue::from(hmac),
            ],
            context,
        )
        .into(),
        None => JsValue::null(),
    })
}

/// `digest(name, bytes, outputLength | undefined)`: `null` for a name that is not an algorithm.
fn digest(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(algorithm) = algorithm_of(args.get_or_undefined(0), context)? else {
        return Ok(JsValue::null());
    };
    let bytes = bytes_of(args.get_or_undefined(1), context)?;
    let length = match args.get_or_undefined(2) {
        length if length.is_undefined() => None,
        length => Some(usize_of(length, context)?),
    };
    let mut hasher = Hasher::new(algorithm, length);
    hasher.update(&bytes);
    uint8_array(&hasher.finish(), context)
}

/// `hmac(name, key, bytes)`: `null` for a name that is not an algorithm with an HMAC.
fn hmac(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(algorithm) = algorithm_of(args.get_or_undefined(0), context)? else {
        return Ok(JsValue::null());
    };
    let key = bytes_of(args.get_or_undefined(1), context)?;
    let bytes = bytes_of(args.get_or_undefined(2), context)?;
    let Some(mut mac) = algorithm::hmac(algorithm, &key) else {
        return Ok(JsValue::null());
    };
    mac.update(&bytes);
    uint8_array(&mac.finish(), context)
}

/// `pbkdf2(name, password, salt, iterations, length)`: `null` for a name that is not an algorithm with an HMAC.
fn pbkdf2(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(algorithm) = algorithm_of(args.get_or_undefined(0), context)? else {
        return Ok(JsValue::null());
    };
    let password = bytes_of(args.get_or_undefined(1), context)?;
    let salt = bytes_of(args.get_or_undefined(2), context)?;
    let iterations = args.get_or_undefined(3).to_u32(context)?;
    let mut out = vec![0; usize_of(args.get_or_undefined(4), context)?];
    if algorithm::pbkdf2(algorithm, &password, &salt, iterations, &mut out).is_none() {
        return Ok(JsValue::null());
    }
    uint8_array(&out, context)
}

/// `scrypt(password, salt, N, r, p, length)`: `null` for parameters scrypt refuses.
fn scrypt(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let password = bytes_of(args.get_or_undefined(0), context)?;
    let salt = bytes_of(args.get_or_undefined(1), context)?;
    let n = args.get_or_undefined(2).to_number(context)?;
    let r = args.get_or_undefined(3).to_u32(context)?;
    let p = args.get_or_undefined(4).to_u32(context)?;
    let length = usize_of(args.get_or_undefined(5), context)?;
    if n < 2.0 || n.fract() != 0.0 || (n as u64) & (n as u64 - 1) != 0 {
        return Ok(JsValue::null());
    }
    let log_n = (n as u64).trailing_zeros() as u8;
    let Ok(params) = scrypt::Params::new(log_n, r, p, length.max(10)) else {
        return Ok(JsValue::null());
    };
    let mut out = vec![0; length];
    if scrypt::scrypt(&password, &salt, &params, &mut out).is_err() {
        return Ok(JsValue::null());
    }
    uint8_array(&out, context)
}

/// `hkdf(name, ikm, salt, info, length)`: `null` for a name without an HMAC, or a length HKDF refuses.
fn hkdf(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(algorithm) = algorithm_of(args.get_or_undefined(0), context)? else {
        return Ok(JsValue::null());
    };
    let ikm = bytes_of(args.get_or_undefined(1), context)?;
    let salt = bytes_of(args.get_or_undefined(2), context)?;
    let info = bytes_of(args.get_or_undefined(3), context)?;
    let length = usize_of(args.get_or_undefined(4), context)?;
    match algorithm::hkdf(algorithm, &ikm, &salt, &info, length) {
        Some(out) => uint8_array(&out, context),
        None => Ok(JsValue::null()),
    }
}

/// `timingSafeEqual(a, b)`: of two views of the same length.
fn timing_safe_equal(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    use subtle::ConstantTimeEq as _;
    let a = bytes_of(args.get_or_undefined(0), context)?;
    let b = bytes_of(args.get_or_undefined(1), context)?;
    Ok(JsValue::from(a.len() == b.len() && bool::from(a.ct_eq(&b))))
}

/// `aesGcm(encrypt, key, iv, data, additionalData, tagLengthBytes)`: the ciphertext and its tag, or the plaintext;
/// `null` where decryption fails or the key is not 16, 24 or 32 bytes.
fn aes_gcm(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    use aes_gcm::aead::consts::{U12, U16};
    use aes_gcm::aead::{AeadInPlace, KeyInit};
    use aes_gcm::{AesGcm, aes::Aes128, aes::Aes192, aes::Aes256};

    let encrypt = args.get_or_undefined(0).to_boolean();
    let key = bytes_of(args.get_or_undefined(1), context)?;
    let iv = bytes_of(args.get_or_undefined(2), context)?;
    let data = bytes_of(args.get_or_undefined(3), context)?;
    let aad = bytes_of(args.get_or_undefined(4), context)?;
    let tag_length = usize_of(args.get_or_undefined(5), context)?;
    if iv.len() != 12 || tag_length != 16 {
        return Err(JsNativeError::error()
            .with_message("AES-GCM is supported with a 96-bit iv and a 128-bit tag on Boa")
            .into());
    }
    fn run<C: KeyInit + AeadInPlace<NonceSize = U12>>(
        key: &[u8],
        iv: &[u8],
        data: &[u8],
        aad: &[u8],
        encrypt: bool,
    ) -> Option<Vec<u8>> {
        let cipher = C::new_from_slice(key).ok()?;
        let nonce = aes_gcm::aead::Nonce::<C>::from_slice(iv);
        let mut buffer = data.to_vec();
        if encrypt {
            cipher.encrypt_in_place(nonce, aad, &mut buffer).ok()?;
        } else {
            cipher.decrypt_in_place(nonce, aad, &mut buffer).ok()?;
        }
        Some(buffer)
    }
    let out = match key.len() {
        16 => run::<AesGcm<Aes128, U12, U16>>(&key, &iv, &data, &aad, encrypt),
        24 => run::<AesGcm<Aes192, U12, U16>>(&key, &iv, &data, &aad, encrypt),
        32 => run::<AesGcm<Aes256, U12, U16>>(&key, &iv, &data, &aad, encrypt),
        _ => None,
    };
    match out {
        Some(out) => uint8_array(&out, context),
        None => Ok(JsValue::null()),
    }
}

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// The natives of crypto.ts, as one object.
pub fn natives(context: &mut Context) -> JsObject {
    let functions: [(&str, Native, usize); 21] = [
        ("randomBytes", random_bytes, 1),
        ("uuidV4", uuid_v4, 0),
        ("uuidV7", uuid_v7, 1),
        ("uuidV5", uuid_v5, 2),
        ("uuidString", uuid_string, 1),
        ("hash", hash, 3),
        ("algorithms", algorithms, 0),
        ("canonicalName", canonical_name, 1),
        ("hasherCreate", hasher_create, 3),
        ("hasherUpdate", hasher_update, 2),
        ("hasherDigest", hasher_digest, 1),
        ("hasherCopy", hasher_copy, 1),
        ("hasherInfo", hasher_info, 1),
        ("digest", digest, 3),
        ("hmac", hmac, 3),
        ("pbkdf2", pbkdf2, 5),
        ("scrypt", scrypt, 6),
        ("hkdf", hkdf, 5),
        ("timingSafeEqual", timing_safe_equal, 2),
        ("aesGcm", aes_gcm, 6),
        ("isXof", is_xof, 1),
    ];
    let mut object = ObjectInitializer::new(context);
    for (name, function, length) in functions {
        object.function(
            NativeFunction::from_fn_ptr(function),
            JsString::from(name),
            length,
        );
    }
    object.build()
}

/// `isXof(name)`.
fn is_xof(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(
        algorithm_of(args.get_or_undefined(0), context)?.is_some_and(Algorithm::is_xof),
    ))
}
