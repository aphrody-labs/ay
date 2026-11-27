//! Web APIs: `src/runtime/webcore` on `bun_js`. The Fetch standard: `Headers`, `Request`, `Response`, `fetch()`; the
//! Encoding standard: `TextEncoder`, `TextDecoder`; the encoders of `Buffer`.

#[path = "Blob.rs"]
pub mod blob;
#[path = "ObjectURLRegistry.rs"]
pub mod object_url_registry;
#[path = "FileSink.rs"]
pub mod file_sink;
#[path = "FormData.rs"]
pub mod form_data;
#[path = "Body.rs"]
pub mod body;
pub mod encoding;
pub mod fetch;
pub mod headers;
#[path = "Request.rs"]
pub mod request;
#[path = "Response.rs"]
pub mod response;
#[path = "TextDecoder.rs"]
pub mod text_decoder;
#[path = "TextEncoder.rs"]
pub mod text_encoder;

use boa_engine::{Context, JsResult};

pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    headers::install(context)?;
    request::install(context)?;
    response::install(context)?;
    text_encoder::install(context)?;
    text_decoder::install(context)?;
    fetch::install(context)
}
