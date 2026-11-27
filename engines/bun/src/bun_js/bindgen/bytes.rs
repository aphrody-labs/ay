//! `b.bytes` and `b.bufferSource`: the bytes of an argument, and the buffers a body reads, writes or returns, with
//! the names of `bun_jsc::bindgen` (`BufferSource`, `JsBytes`, `StringOrJsBytes`). Boa has no pointer to a string's
//! code units that outlives the call to read it: what a body borrows is a copy the call keeps.

use boa_engine::JsObject;
use boa_engine::object::builtins::{JsArrayBuffer, JsDataView, JsSharedArrayBuffer, JsTypedArray, JsUint8Array};

use super::{JsHeap, JsLocal, Result};
use crate::{JSGlobalObject, JSValue, JsResult};

/// What a [`JsLocal`] of a typed array, a `DataView`, an `ArrayBuffer` or a `SharedArrayBuffer` is.
pub enum BufferSource {}

/// Where the bytes a buffer source shows are: its buffer and the range of it.
enum Region {
    Plain(JsArrayBuffer, usize, usize),
    Shared(JsSharedArrayBuffer, usize, usize),
    /// Detached, out of bounds, or not a buffer source: no bytes.
    Empty,
}

fn buffer_region(buffer: JsObject, offset: usize, length: usize) -> Region {
    if let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) {
        return Region::Plain(buffer, offset, length);
    }
    match JsSharedArrayBuffer::from_object(buffer) {
        Ok(shared) => Region::Shared(shared, offset, length),
        Err(_) => Region::Empty,
    }
}

fn region(global: &JSGlobalObject, value: &JSValue) -> Region {
    let Some(object) = value.0.as_object() else {
        return Region::Empty;
    };
    let view = |buffer: boa_engine::JsResult<boa_engine::JsValue>, offset: Option<usize>, length: Option<usize>| {
        match (buffer.ok().and_then(|buffer| buffer.as_object()), offset, length) {
            (Some(buffer), Some(offset), Some(length)) => buffer_region(buffer, offset, length),
            _ => Region::Empty,
        }
    };
    if let Ok(array) = JsTypedArray::from_object(object.clone()) {
        let buffer = array.buffer(global.cx());
        let offset = array.byte_offset(global.cx()).ok();
        let length = array.byte_length(global.cx()).ok();
        return view(buffer, offset, length);
    }
    if let Ok(data_view) = JsDataView::from_object(object.clone()) {
        let buffer = data_view.buffer(global.cx()).map(Into::into);
        let offset = data_view.byte_offset(global.cx()).ok().map(|n| n as usize);
        let length = data_view.byte_length(global.cx()).ok().map(|n| n as usize);
        return view(buffer, offset, length);
    }
    if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
        let length = buffer.data().map_or(0, |data| data.len());
        return Region::Plain(buffer, 0, length);
    }
    match JsSharedArrayBuffer::from_object(object) {
        Ok(shared) => {
            let length = shared.byte_length();
            Region::Shared(shared, 0, length)
        }
        Err(_) => Region::Empty,
    }
}

/// `offset..offset + length` within `len`, empty where it is out of bounds.
fn clamp(offset: usize, length: usize, len: usize) -> core::ops::Range<usize> {
    if offset.checked_add(length).is_none_or(|end| end > len) {
        return 0..0;
    }
    offset..offset + length
}

/// The bytes a buffer source shows, as they are now: empty for a detached or an out-of-bounds view.
pub(crate) fn read_bytes(global: &JSGlobalObject, value: &JSValue) -> Vec<u8> {
    match region(global, value) {
        Region::Plain(buffer, offset, length) => buffer
            .data()
            .map(|data| data[clamp(offset, length, data.len())].to_vec())
            .unwrap_or_default(),
        Region::Shared(shared, offset, length) => {
            let bytes = shared.to_vec();
            bytes[clamp(offset, length, bytes.len())].to_vec()
        }
        Region::Empty => Vec::new(),
    }
}

impl<'a> JsLocal<'a, BufferSource> {
    /// `f` of the bytes the buffer shows now. A shared buffer's are a copy.
    pub fn with_bytes<R>(&self, heap: &JsHeap<'a>, f: impl FnOnce(&[u8]) -> R) -> R {
        match region(heap.global_object(), &self.0) {
            Region::Plain(buffer, offset, length) => match buffer.data() {
                Some(data) => f(&data[clamp(offset, length, data.len())]),
                None => f(&[]),
            },
            Region::Shared(shared, offset, length) => {
                let bytes = shared.to_vec();
                f(&bytes[clamp(offset, length, bytes.len())])
            }
            Region::Empty => f(&[]),
        }
    }

    /// `f` of the bytes the buffer shows, to write. A shared buffer's are copied out and back.
    ///
    /// # Safety
    /// `f` uses no bytes borrowed from another argument, which can be the same memory.
    pub unsafe fn with_bytes_mut<R>(&self, heap: &JsHeap<'a>, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let global = heap.global_object();
        match region(global, &self.0) {
            Region::Plain(buffer, offset, length) => match buffer.data_mut() {
                Some(mut data) => {
                    let range = clamp(offset, length, data.len());
                    f(&mut data[range])
                }
                None => f(&mut []),
            },
            Region::Shared(shared, offset, length) => {
                let mut bytes = shared.to_vec();
                let range = clamp(offset, length, bytes.len());
                let result = f(&mut bytes[range.clone()]);
                if let Ok(view) = JsUint8Array::from_shared_array_buffer(shared, global.cx()) {
                    for index in range {
                        let _ = view.set(index, bytes[index], true, global.cx());
                    }
                }
                result
            }
            Region::Empty => f(&mut []),
        }
    }

    /// How many bytes it shows now.
    pub fn byte_length(&self, heap: &JsHeap<'a>) -> usize {
        self.with_bytes(heap, <[u8]>::len)
    }
}

impl<'a> JsHeap<'a> {
    /// A new `Buffer` of `bytes`.
    pub fn create_buffer(&self, bytes: &[u8]) -> Result<'a, JsLocal<'a, BufferSource>> {
        Ok(JsLocal::from_raw(JSValue::create_buffer(self.global, bytes)?))
    }

    pub fn create_buffer_from_box(&self, bytes: Box<[u8]>) -> Result<'a, JsLocal<'a, BufferSource>> {
        self.create_buffer(&bytes)
    }

    /// A new `Uint8Array` of `bytes`.
    pub fn create_uint8_array(&self, bytes: &[u8]) -> Result<'a, JsLocal<'a, BufferSource>> {
        let array = JsUint8Array::from_iter(bytes.iter().copied(), self.global.cx());
        let array = self.global.lift(array)?;
        Ok(JsLocal::from_raw(JSValue(array.into())))
    }

    /// A new `ArrayBuffer` of `bytes`.
    pub fn create_array_buffer(&self, bytes: &[u8]) -> Result<'a, JsLocal<'a, BufferSource>> {
        let block = boa_engine::object::builtins::AlignedVec::from_iter(0, bytes.iter().copied());
        let buffer = JsArrayBuffer::from_byte_block(block, self.global.cx());
        let buffer = self.global.lift(buffer)?;
        Ok(JsLocal::from_raw(JSValue(buffer.into())))
    }
}

/// A string as a body reads it (`b.string`, and `string: "asIs"` of `b.bytes`): what `bun_core::String` is to a
/// body of the JavaScriptCore build.
#[derive(Clone)]
pub struct JsStr<'a>(JSValue, super::Invariant<'a>);

impl<'a> JsStr<'a> {
    pub(crate) fn new(value: JSValue) -> Self {
        JsStr(value, core::marker::PhantomData)
    }

    /// Its UTF-8, a lone surrogate as U+FFFD.
    pub fn to_utf8(&self) -> Vec<u8> {
        self.0
            .0
            .as_string()
            .map(|string| string.to_std_string_lossy().into_bytes())
            .unwrap_or_default()
    }

    /// Its UTF-16 code units.
    pub fn to_utf16(&self) -> Vec<u16> {
        self.0.0.as_string().map(|string| string.iter().collect()).unwrap_or_default()
    }

    /// How many UTF-16 code units it has.
    pub fn length(&self) -> usize {
        self.0.0.as_string().map_or(0, |string| string.len())
    }

    pub fn is_empty(&self) -> bool {
        self.length() == 0
    }

    /// The string value.
    pub fn value(&self) -> &JSValue {
        &self.0
    }
}

/// `held: "handle"` of `b.bytes`: a string as its UTF-8, converted in the call, or a buffer, whose bytes `get` reads
/// when the body asks.
#[derive(Clone)]
pub struct JsBytes<'a>(Held<'a>);

#[derive(Clone)]
enum Held<'a> {
    Kept(&'a [u8]),
    Buffer(JSValue),
}

impl<'a> JsBytes<'a> {
    pub(crate) fn kept(bytes: &'a [u8]) -> Self {
        JsBytes(Held::Kept(bytes))
    }

    pub(crate) fn of_buffer(buffer: JSValue) -> Self {
        JsBytes(Held::Buffer(buffer))
    }

    /// The bytes now.
    pub fn get(&self, heap: &JsHeap<'a>) -> &'a [u8] {
        match &self.0 {
            Held::Kept(bytes) => bytes,
            Held::Buffer(buffer) => heap.keep(read_bytes(heap.global_object(), buffer)),
        }
    }

    /// As [`get`](Self::get): a shared buffer's bytes are a copy already.
    pub fn get_or_copy_if_shared(&self, heap: &JsHeap<'a>) -> &'a [u8] {
        self.get(heap)
    }

    /// The buffer, `None` for a string.
    pub fn buffer(&self) -> Option<&JSValue> {
        match &self.0 {
            Held::Buffer(buffer) => Some(buffer),
            Held::Kept(_) => None,
        }
    }
}

/// `string: "asIs", held: "handle"` of `b.bytes`: a string as it is, or a buffer.
#[derive(Clone)]
pub enum StringOrJsBytes<'a> {
    String(JsStr<'a>),
    Bytes(JsBytes<'a>),
}

impl<'a> StringOrJsBytes<'a> {
    /// The bytes the body owns: a string's UTF-8.
    pub fn into_vec(self, heap: &JsHeap<'a>) -> Vec<u8> {
        match self {
            StringOrJsBytes::String(string) => string.to_utf8(),
            StringOrJsBytes::Bytes(bytes) => bytes.get(heap).to_vec(),
        }
    }
}

/// What `Blob__sharedView` says of a value: the bytes of a `Blob` in memory, a `Blob` that is a file, or no `Blob`.
pub enum BlobBytes {
    NotBlob,
    InMemory(Vec<u8>),
    NotInMemory,
}

/// What reads a `Blob`'s bytes for `from: ["blob"]`: the crate that defines `Blob` puts it on the realm.
pub type BlobReader = fn(&JSGlobalObject, &JSValue) -> JsResult<BlobBytes>;

impl JSGlobalObject {
    pub fn set_blob_reader(&self, reader: BlobReader) {
        self.state().blob_reader = Some(reader);
    }

    pub(crate) fn blob_bytes(&self, value: &JSValue) -> JsResult<BlobBytes> {
        match self.state().blob_reader {
            Some(reader) if value.is_object() => reader(self, value),
            _ => Ok(BlobBytes::NotBlob),
        }
    }
}

impl JSValue {
    /// An `ArrayBuffer` or a `SharedArrayBuffer`.
    pub fn is_array_buffer(&self) -> bool {
        self.0.as_object().is_some_and(|object| {
            JsArrayBuffer::from_object(object.clone()).is_ok() || JsSharedArrayBuffer::from_object(object).is_ok()
        })
    }

    /// A `SharedArrayBuffer`, or a view of one.
    pub fn is_shared_buffer_source(&self, global: &JSGlobalObject) -> bool {
        matches!(region(global, self), Region::Shared(..))
    }
}
