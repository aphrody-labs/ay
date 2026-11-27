//! Web APIs: what WebCore implements in src/jsc/bindings/webcore for the JavaScriptCore build.

pub mod abort;
pub mod message_port;
pub mod performance;
pub mod streams;
pub mod worker;

use boa_engine::{Context, JsObject, JsResult};
use boa_wintertc::store::HostSerialized;

/// The serialization steps of the realm's platform objects (`structuredClone`, `postMessage`): a `MessagePort` that
/// is transferred, a `Blob` or a `File`.
pub(crate) fn serialize_host(object: &JsObject, context: &mut Context) -> JsResult<Option<Box<dyn HostSerialized>>> {
    if let Some(port) = message_port::serialize_port(object, context)? {
        return Ok(Some(port));
    }
    crate::webcore::blob::serialize_host(object, context)
}
