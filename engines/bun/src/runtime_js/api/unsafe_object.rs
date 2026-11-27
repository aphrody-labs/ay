//! The bodies of src/runtime/api/UnsafeObject.bind.ts (`Bun.unsafe`) on `bun_js`: src/runtime/api/UnsafeObject.rs.
//! Boa has no JIT and no garbage collection levels: `setJITPolicy` validates its argument and does nothing else,
//! `gcAggressionLevel` keeps the level it is given.

use std::sync::atomic::{AtomicI32, Ordering};

use bun_js::bindgen::{JsLocal, JsScope};
use bun_js::{ErrorCode, JSValue, JsResult};

use crate::node::types::invalid_arguments;

static GC_LEVEL: AtomicI32 = AtomicI32::new(0);

/// `Bun.unsafe.gcAggressionLevel(level?)`: the level before; 0, 1 and 2 are levels, anything else changes nothing.
pub(crate) fn gc_aggression_level(level: Option<f64>) -> i32 {
    match level.map(|level| level as i32) {
        Some(level @ 0..=2) => GC_LEVEL.swap(level, Ordering::Relaxed),
        _ => GC_LEVEL.load(Ordering::Relaxed),
    }
}

/// `Bun.unsafe.arrayBufferToString(buffer)`: the bytes as UTF-16 code units for a `Uint16Array` or an `Int16Array`,
/// as Latin-1 for anything else.
pub(crate) fn array_buffer_to_string<'a>(cx: &mut JsScope<'a>, buffer: JsLocal<'a>) -> JsResult<JSValue> {
    let global = cx.global();
    let buffer = buffer.value();
    let Some(bytes) = buffer.array_buffer_or_view_bytes(global)? else {
        return Err(invalid_arguments(global, format_args!("Expected an ArrayBuffer")));
    };
    let units: Vec<u16> = if buffer.is_16_bit_integer_array() {
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    } else {
        bytes.iter().map(|&byte| u16::from(byte)).collect()
    };
    Ok(JSValue::from_utf16(&units))
}

/// `Bun.unsafe.mimallocDump()`: this build allocates with Rust's global allocator, which keeps no statistics.
pub(crate) fn mimalloc_dump() {}

/// `Bun.unsafe.memoryFootprint()`: `Pss:` of `/proc/self/smaps_rollup` on Linux (c-bindings.cpp's
/// `Bun__memoryFootprint`), the resident set elsewhere; `undefined` where neither can be read.
pub(crate) fn memory_footprint() -> Option<usize> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let rollup = bun_sys::File::read_from(bun_sys::Fd::cwd(), b"/proc/self/smaps_rollup").ok()?;
        let line = bun_core::strings::index_of(&rollup, b"\nPss:")?;
        let rest = &rollup[line + b"\nPss:".len()..];
        let digits: Vec<u8> = rest
            .iter()
            .copied()
            .skip_while(|byte| *byte == b' ' || *byte == b'\t')
            .take_while(u8::is_ascii_digit)
            .collect();
        let kilobytes: usize = std::str::from_utf8(&digits).ok()?.parse().ok()?;
        Some(kilobytes * 1024).filter(|&bytes| bytes > 0)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        bun_sys::self_process_memory_usage().filter(|&bytes| bytes > 0)
    }
}

/// `Bun.unsafe.setJITPolicy(scale)`: a finite number >= 1.
pub(crate) fn set_jit_policy(cx: &mut JsScope<'_>, scale: f64) -> JsResult<()> {
    let global = cx.global();
    if !(scale.is_finite() && scale >= 1.0) {
        let shown = JSValue::js_number(scale).to_utf8(global)?;
        return Err(ErrorCode::OUT_OF_RANGE.throw(
            global,
            format_args!("The value of \"scale\" is out of range. It must be a finite number >= 1. Received {shown}"),
        ));
    }
    Ok(())
}
