//! `URL.createObjectURL`, `URL.revokeObjectURL` and node:buffer's `resolveObjectURL`: src/runtime/webcore/
//! ObjectURLRegistry.rs on `bun_js`. The registry belongs to the realm's thread: a Blob's store is not `Send`, and
//! Boa runs no workers.

use std::cell::RefCell;
use std::collections::HashMap;

use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsResult};

use super::blob::Blob;

const PREFIX: &[u8] = b"blob:";
const UUID_LENGTH: usize = 36;

thread_local! {
    static REGISTRY: RefCell<HashMap<[u8; 16], Blob>> = RefCell::new(HashMap::new());
}

/// A version 4 UUID, as `bun_jsc::UUID::init`.
fn next_uuid() -> Option<[u8; 16]> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Some(bytes)
}

fn print_uuid(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(UUID_LENGTH);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `UUID::parse`: exactly `8-4-4-4-12` hex digits.
fn parse_uuid(text: &[u8]) -> Option<[u8; 16]> {
    if text.len() != UUID_LENGTH
        || text[8] != b'-'
        || text[13] != b'-'
        || text[18] != b'-'
        || text[23] != b'-'
    {
        return None;
    }
    let mut bytes = [0u8; 16];
    let mut at = 0;
    for byte in &mut bytes {
        if matches!(at, 8 | 13 | 18 | 23) {
            at += 1;
        }
        let high = (text[at] as char).to_digit(16)?;
        let low = (text[at + 1] as char).to_digit(16)?;
        *byte = (high * 16 + low) as u8;
        at += 2;
    }
    Some(bytes)
}

/// The Blob that `blob:<uuid>` names, a copy of the one registered: `fetch()` and the module loader read it.
pub fn resolve(url: &[u8]) -> Option<Blob> {
    let uuid = parse_uuid(url.strip_prefix(PREFIX)?)?;
    REGISTRY.with(|registry| registry.borrow().get(&uuid).cloned())
}

/// Whether `url` is long enough to be a `blob:` URL of this registry.
pub fn is_blob_url(url: &[u8]) -> bool {
    url.len() >= PREFIX.len() + UUID_LENGTH && url.starts_with(PREFIX)
}

/// `URL.createObjectURL(blob)`.
pub fn create_object_url(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let Some(blob) = frame.argument(0).as_class_ref::<Blob>() else {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("object must be of type Blob or BuildArtifact"),
        ));
    };
    let blob = blob.borrow(global)?.clone();
    let Some(uuid) = next_uuid() else {
        return Err(global.throw(format_args!("createObjectURL: the OS gave no random bytes")));
    };
    REGISTRY.with(|registry| registry.borrow_mut().insert(uuid, blob));
    Ok(JSValue::from_utf8(format!("blob:{}", print_uuid(&uuid)).as_bytes()))
}

/// `URL.revokeObjectURL(url)`: a string or a `String` object; what is not a URL of the registry is ignored.
pub fn revoke_object_url(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    if frame.arguments_count() < 1 {
        return Err(ErrorCode::MISSING_ARGS.throw(
            global,
            format_args!("The \"url\" argument must be specified"),
        ));
    }
    let url = frame.argument(0);
    let url = match crate::engine::string_object_value(&url) {
        Some(string) => string,
        None if url.is_string() => url,
        None => {
            return Err(ErrorCode::INVALID_ARG_TYPE
                .throw(global, format_args!("revokeObjectURL expects a string")));
        }
    };
    let url = url.to_utf8(global)?;
    if let Some(uuid) = url.as_bytes().strip_prefix(PREFIX).and_then(parse_uuid) {
        REGISTRY.with(|registry| registry.borrow_mut().remove(&uuid));
    }
    Ok(JSValue::UNDEFINED)
}

/// node:buffer's `resolveObjectURL(url)`: a copy of the Blob, or `undefined`; nothing throws.
pub fn resolve_object_url(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let url = frame.argument(0).to_utf8(global)?;
    match is_blob_url(url.as_bytes()).then(|| resolve(url.as_bytes())).flatten() {
        Some(blob) => blob.to_js(global),
        None => Ok(JSValue::UNDEFINED),
    }
}
