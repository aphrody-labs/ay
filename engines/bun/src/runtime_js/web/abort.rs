//! `DOMException`, `AbortSignal.abort()`, `AbortSignal.timeout()` and `AbortSignal.any()`: abort.js, over Boa's
//! AbortController and AbortSignal, after the timers it sets.

use boa_engine::{Context, JsResult, Source};

pub fn install(context: &mut Context) -> JsResult<()> {
    context.eval(Source::from_bytes(include_str!("abort.js"))).map(drop)
}
