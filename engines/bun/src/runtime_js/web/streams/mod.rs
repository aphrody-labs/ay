//! WHATWG Streams: src/jsc/bindings/webcore/streams on `bun_js`, the same classes and spec steps in Rust.
//!
//! Not ported yet: the byte-stream `tee()` (a byte stream tees as a default one), `readMany()`, brotli, and Bun's
//! native sources (BunStreamSource.cpp) beyond a descriptor's reads (webcore::blob::file_sink).

pub mod direct_stream_controller;
pub mod internals;
pub mod native_transform;
pub mod queuing_strategy;
pub mod readable_byte_stream_controller;
pub mod readable_stream;
pub mod readable_stream_byob_reader;
pub mod readable_stream_async_iterator;
pub mod readable_stream_default_controller;
pub mod readable_stream_default_reader;
pub mod stream_pipe_to_operation;
pub mod stream_tee_state;
pub mod transform_stream;
pub mod transform_stream_default_controller;
pub mod writable_stream;
pub mod writable_stream_default_controller;
pub mod writable_stream_default_writer;

use boa_engine::{Context, JsResult};
use bun_js::JSGlobalObject;
use bun_js::class::constructor;

use crate::{define_global, lift};

/// Puts the streams classes in the realm.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let global = JSGlobalObject::from_context(context);
    let classes = [
        ("ReadableStream", lift(global, readable_stream::constructor(global))?),
        (
            "ReadableStreamDefaultReader",
            lift(global, constructor::<readable_stream_default_reader::ReadableStreamDefaultReader>(global))?,
        ),
        (
            "ReadableStreamDefaultController",
            lift(global, constructor::<readable_stream_default_controller::ReadableStreamDefaultController>(global))?,
        ),
        (
            "ReadableByteStreamController",
            lift(global, constructor::<readable_byte_stream_controller::ReadableByteStreamController>(global))?,
        ),
        (
            "ReadableStreamBYOBReader",
            lift(global, constructor::<readable_stream_byob_reader::ReadableStreamBYOBReader>(global))?,
        ),
        (
            "ReadableStreamBYOBRequest",
            lift(global, constructor::<readable_byte_stream_controller::ReadableStreamBYOBRequest>(global))?,
        ),
        ("WritableStream", lift(global, constructor::<writable_stream::WritableStream>(global))?),
        (
            "WritableStreamDefaultWriter",
            lift(global, constructor::<writable_stream_default_writer::WritableStreamDefaultWriter>(global))?,
        ),
        (
            "WritableStreamDefaultController",
            lift(global, constructor::<writable_stream_default_controller::WritableStreamDefaultController>(global))?,
        ),
        ("TransformStream", lift(global, constructor::<transform_stream::TransformStream>(global))?),
        (
            "TransformStreamDefaultController",
            lift(global, constructor::<transform_stream_default_controller::TransformStreamDefaultController>(global))?,
        ),
        ("CountQueuingStrategy", lift(global, constructor::<queuing_strategy::CountQueuingStrategy>(global))?),
        (
            "ByteLengthQueuingStrategy",
            lift(global, constructor::<queuing_strategy::ByteLengthQueuingStrategy>(global))?,
        ),
        ("TextEncoderStream", lift(global, constructor::<native_transform::TextEncoderStream>(global))?),
        ("TextDecoderStream", lift(global, constructor::<native_transform::TextDecoderStream>(global))?),
        ("CompressionStream", lift(global, constructor::<native_transform::CompressionStream>(global))?),
        ("DecompressionStream", lift(global, constructor::<native_transform::DecompressionStream>(global))?),
    ];
    for (name, value) in classes {
        define_global(context, name, value.into_engine())?;
    }
    Ok(())
}
