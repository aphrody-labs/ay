//! The bytes of an `ArrayBuffer`, a typed array or a `DataView` (`bun_jsc::ArrayBuffer`), the code units of a
//! string, and the typed arrays and `Buffer`s Rust makes.

use boa_engine::builtins::typed_array::TypedArrayKind;
use boa_engine::object::builtins::{
    AlignedVec, ExternalBytes, JsArrayBuffer, JsDataView, JsSharedArrayBuffer, JsTypedArray,
    JsUint8Array,
};
use boa_engine::string::JsStrVariant;
use boa_engine::{JsObject, JsString, JsValue};

use crate::{JSGlobalObject, JSValue, JsResult};

/// What a view is, as `bun_jsc::JSType` names the typed array types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedArrayType {
    ArrayBuffer,
    DataView,
    Int8Array,
    Uint8Array,
    Uint8ClampedArray,
    Int16Array,
    Uint16Array,
    Int32Array,
    Uint32Array,
    Float16Array,
    Float32Array,
    Float64Array,
    BigInt64Array,
    BigUint64Array,
}

impl TypedArrayType {
    /// The size of an element, 1 for an `ArrayBuffer` and a `DataView`.
    pub fn element_size(self) -> usize {
        match self {
            TypedArrayType::Int16Array
            | TypedArrayType::Uint16Array
            | TypedArrayType::Float16Array => 2,
            TypedArrayType::Int32Array
            | TypedArrayType::Uint32Array
            | TypedArrayType::Float32Array => 4,
            TypedArrayType::Float64Array
            | TypedArrayType::BigInt64Array
            | TypedArrayType::BigUint64Array => 8,
            _ => 1,
        }
    }

    pub fn is_typed_array(self) -> bool {
        !matches!(self, TypedArrayType::ArrayBuffer | TypedArrayType::DataView)
    }
}

fn typed_array_type(kind: TypedArrayKind) -> TypedArrayType {
    match kind {
        TypedArrayKind::Int8 => TypedArrayType::Int8Array,
        TypedArrayKind::Uint8 => TypedArrayType::Uint8Array,
        TypedArrayKind::Uint8Clamped => TypedArrayType::Uint8ClampedArray,
        TypedArrayKind::Int16 => TypedArrayType::Int16Array,
        TypedArrayKind::Uint16 => TypedArrayType::Uint16Array,
        TypedArrayKind::Int32 => TypedArrayType::Int32Array,
        TypedArrayKind::Uint32 => TypedArrayType::Uint32Array,
        TypedArrayKind::BigInt64 => TypedArrayType::BigInt64Array,
        TypedArrayKind::BigUint64 => TypedArrayType::BigUint64Array,
        TypedArrayKind::Float32 => TypedArrayType::Float32Array,
        TypedArrayKind::Float64 => TypedArrayType::Float64Array,
        #[allow(unreachable_patterns)]
        _ => TypedArrayType::Float16Array,
    }
}

#[derive(Clone, Debug)]
enum Backing {
    Plain(JsArrayBuffer),
    Shared(JsSharedArrayBuffer),
}

/// The bytes a value views, from its byte offset for its byte length as they are when it is read: a view of a
/// detached buffer has none.
#[derive(Clone, Debug)]
pub struct ArrayBuffer {
    backing: Backing,
    byte_offset: usize,
    byte_len: usize,
    typed_array_type: TypedArrayType,
}

fn backing(buffer: JsObject) -> Option<Backing> {
    if let Ok(plain) = JsArrayBuffer::from_object(buffer.clone()) {
        return Some(Backing::Plain(plain));
    }
    JsSharedArrayBuffer::from_object(buffer)
        .ok()
        .map(Backing::Shared)
}

impl ArrayBuffer {
    pub fn typed_array_type(&self) -> TypedArrayType {
        self.typed_array_type
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    pub fn byte_offset(&self) -> usize {
        self.byte_offset
    }

    /// Its length in elements: a typed array's `length`.
    pub fn len(&self) -> usize {
        self.byte_len / self.typed_array_type.element_size()
    }

    pub fn is_empty(&self) -> bool {
        self.byte_len == 0
    }

    pub fn is_shared(&self) -> bool {
        matches!(self.backing, Backing::Shared(_))
    }

    pub fn is_detached(&self) -> bool {
        match &self.backing {
            Backing::Plain(buffer) => buffer.data().is_none(),
            Backing::Shared(_) => false,
        }
    }

    /// Whether both view the same `ArrayBuffer`.
    pub fn same_buffer(&self, other: &ArrayBuffer) -> bool {
        let (a, b): (JsObject, JsObject) = match (&self.backing, &other.backing) {
            (Backing::Plain(a), Backing::Plain(b)) => (a.clone().into(), b.clone().into()),
            (Backing::Shared(a), Backing::Shared(b)) => (a.clone().into(), b.clone().into()),
            _ => return false,
        };
        JsObject::equals(&a, &b)
    }

    /// Calls `f` with the bytes. A shared buffer's are a copy: another thread may write them meanwhile.
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let range = self.byte_offset..self.byte_offset + self.byte_len;
        match &self.backing {
            Backing::Plain(buffer) => match buffer.data() {
                Some(data) => f(data.get(range).unwrap_or_default()),
                None => f(&[]),
            },
            Backing::Shared(buffer) => f(buffer.to_vec().get(range).unwrap_or_default()),
        }
    }

    /// Calls `f` with the bytes to write. Nothing else may read or write the same buffer meanwhile: copy what `f`
    /// reads from it first.
    pub fn with_bytes_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let range = self.byte_offset..self.byte_offset + self.byte_len;
        match &self.backing {
            Backing::Plain(buffer) => match buffer.data_mut() {
                Some(mut data) => f(data.get_mut(range).unwrap_or_default()),
                None => f(&mut []),
            },
            // `f` writes a copy; only the bytes it changed are stored back, so it overwrites no other write.
            Backing::Shared(buffer) => {
                let before = buffer.to_vec().get(range).unwrap_or_default().to_vec();
                let mut bytes = before.clone();
                let result = f(&mut bytes);
                buffer.store_changed(self.byte_offset, &before, &bytes);
                result
            }
        }
    }
}

/// The code units of a string: Boa keeps one whose units all fit in a byte as Latin-1.
#[derive(Clone, Copy, Debug)]
pub enum StringUnits<'a> {
    Latin1(&'a [u8]),
    Utf16(&'a [u16]),
}

impl StringUnits<'_> {
    pub fn len(&self) -> usize {
        match self {
            StringUnits::Latin1(units) => units.len(),
            StringUnits::Utf16(units) => units.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl JSValue {
    /// The bytes of an `ArrayBuffer`, a `SharedArrayBuffer`, a typed array or a `DataView`.
    pub fn as_array_buffer(&self, global: &JSGlobalObject) -> JsResult<Option<ArrayBuffer>> {
        let Some(object) = self.0.as_object() else {
            return Ok(None);
        };
        if let Ok(array) = JsTypedArray::from_object(object.clone()) {
            let Some(kind) = array.kind() else {
                return Ok(None);
            };
            let buffer = global.lift(array.buffer(global.cx()))?;
            let byte_offset = global.lift(array.byte_offset(global.cx()))?;
            let byte_len = global.lift(array.byte_length(global.cx()))?;
            let Some(backing) = buffer.as_object().and_then(backing) else {
                return Ok(None);
            };
            return Ok(Some(ArrayBuffer {
                backing,
                byte_offset,
                byte_len,
                typed_array_type: typed_array_type(kind),
            }));
        }
        if let Ok(view) = JsDataView::from_object(object.clone()) {
            let buffer = global.lift(view.buffer(global.cx()))?;
            let Some(backing) = buffer.as_object().and_then(backing) else {
                return Ok(None);
            };
            // A DataView of a detached buffer throws for its offset and length; it views nothing.
            let byte_offset = view.byte_offset(global.cx()).unwrap_or(0) as usize;
            let byte_len = view.byte_length(global.cx()).unwrap_or(0) as usize;
            return Ok(Some(ArrayBuffer {
                backing,
                byte_offset,
                byte_len,
                typed_array_type: TypedArrayType::DataView,
            }));
        }
        let Some(backing) = backing(object) else {
            return Ok(None);
        };
        let byte_len = match &backing {
            Backing::Plain(buffer) => buffer.data().map_or(0, |data| data.len()),
            Backing::Shared(buffer) => buffer.byte_length(),
        };
        Ok(Some(ArrayBuffer {
            backing,
            byte_offset: 0,
            byte_len,
            typed_array_type: TypedArrayType::ArrayBuffer,
        }))
    }

    /// A new `Uint8Array` of a copy of `bytes`.
    pub fn create_uint8_array(global: &JSGlobalObject, bytes: &[u8]) -> JsResult<JSValue> {
        let block = AlignedVec::from_slice(0, bytes);
        let buffer = global.lift(JsArrayBuffer::from_byte_block(block, global.cx()))?;
        let array = global.lift(JsUint8Array::from_array_buffer(buffer, global.cx()))?;
        Ok(JSValue(array.into()))
    }

    /// A string of Latin-1 code units.
    pub fn from_latin1(units: &[u8]) -> JSValue {
        JSValue(JsString::from(boa_engine::string::JsStr::latin1(units)).into())
    }

    /// Calls `f` with the code units of a string, without copying them; `None` for anything else.
    pub fn with_string_units<R>(&self, f: impl FnOnce(StringUnits<'_>) -> R) -> Option<R> {
        let string = self.0.as_string()?;
        Some(match string.variant() {
            JsStrVariant::Latin1(units) => f(StringUnits::Latin1(units)),
            JsStrVariant::Utf16(units) => f(StringUnits::Utf16(units)),
        })
    }
}

impl JSValue {
    /// A copy of the bytes a typed array or a `DataView` views, as C++ reads a `JSArrayBufferView`: `None` for
    /// anything else, and for a view whose buffer is detached.
    pub fn array_buffer_view_bytes(&self, global: &JSGlobalObject) -> JsResult<Option<Vec<u8>>> {
        let Some(object) = self.0.as_object() else {
            return Ok(None);
        };
        if let Ok(array) = JsTypedArray::from_object(object.clone()) {
            let buffer = array.buffer(global.cx());
            let buffer = global.lift(buffer)?;
            let Some(bytes) = buffer_bytes(global, buffer.as_object().as_ref())? else {
                return Ok(None);
            };
            let offset = array.byte_offset(global.cx());
            let offset = global.lift(offset)?;
            let length = array.byte_length(global.cx());
            let length = global.lift(length)?;
            return Ok(bytes.get(offset..offset + length).map(<[u8]>::to_vec));
        }
        if let Ok(view) = JsDataView::from_object(object.clone()) {
            let buffer = view.buffer(global.cx());
            let buffer = global.lift(buffer)?;
            let Some(bytes) = buffer_bytes(global, buffer.as_object().as_ref())? else {
                return Ok(None);
            };
            let offset = view.byte_offset(global.cx());
            let offset = global.lift(offset)? as usize;
            let length = view.byte_length(global.cx());
            let length = global.lift(length)? as usize;
            return Ok(bytes.get(offset..offset + length).map(<[u8]>::to_vec));
        }
        Ok(None)
    }

    /// The bytes of a view, as [`array_buffer_view_bytes`](Self::array_buffer_view_bytes), or of an `ArrayBuffer` or
    /// a `SharedArrayBuffer` itself: what node:fs takes as data or as a path.
    pub fn array_buffer_or_view_bytes(&self, global: &JSGlobalObject) -> JsResult<Option<Vec<u8>>> {
        if self.is_array_buffer_view() {
            return self.array_buffer_view_bytes(global);
        }
        buffer_bytes(global, self.0.as_object().as_ref())
    }

    /// How many bytes a typed array or a `DataView` views; `None` for anything else.
    pub fn array_buffer_view_byte_length(&self, global: &JSGlobalObject) -> JsResult<Option<usize>> {
        let Some(object) = self.0.as_object() else {
            return Ok(None);
        };
        if let Ok(array) = JsTypedArray::from_object(object.clone()) {
            let length = array.byte_length(global.cx());
            return global.lift(length).map(Some);
        }
        if let Ok(view) = JsDataView::from_object(object) {
            let length = view.byte_length(global.cx());
            return global.lift(length).map(|n| Some(n as usize));
        }
        Ok(None)
    }

    /// Whether this is a `Uint16Array` or an `Int16Array`: the typed arrays whose bytes are UTF-16 code units.
    pub fn is_16_bit_integer_array(&self) -> bool {
        use boa_engine::builtins::typed_array::TypedArrayKind;
        self.0
            .as_object()
            .and_then(|object| JsTypedArray::from_object(object).ok()?.kind())
            .is_some_and(|kind| matches!(kind, TypedArrayKind::Uint16 | TypedArrayKind::Int16))
    }

    /// Whether this is a `Uint8Array` (a `Buffer` is one), a `DataView` or an `ArrayBuffer`: the buffers that
    /// node:fs takes a path in.
    pub fn is_uint8_array_data_view_or_array_buffer(&self) -> bool {
        self.0.as_object().is_some_and(|object| {
            JsUint8Array::from_object(object.clone()).is_ok()
                || JsDataView::from_object(object.clone()).is_ok()
                || JsArrayBuffer::from_object(object).is_ok()
        })
    }

    /// Copies `bytes` into the view at `offset`, as much as fits: how many were copied. A view of a detached or a
    /// shared buffer takes none.
    pub fn write_array_buffer_view(&self, global: &JSGlobalObject, offset: usize, bytes: &[u8]) -> JsResult<usize> {
        let Some(object) = self.0.as_object() else {
            return Ok(0);
        };
        let (buffer, view_offset, view_length) = if let Ok(array) = JsTypedArray::from_object(object.clone()) {
            let buffer = array.buffer(global.cx());
            let buffer = global.lift(buffer)?;
            let view_offset = array.byte_offset(global.cx());
            let view_offset = global.lift(view_offset)?;
            let view_length = array.byte_length(global.cx());
            (buffer, view_offset, global.lift(view_length)?)
        } else if let Ok(view) = JsDataView::from_object(object) {
            let buffer = view.buffer(global.cx());
            let buffer = global.lift(buffer)?;
            let view_offset = view.byte_offset(global.cx());
            let view_offset = global.lift(view_offset)? as usize;
            let view_length = view.byte_length(global.cx());
            (buffer, view_offset, global.lift(view_length)? as usize)
        } else {
            return Ok(0);
        };
        let Some(buffer) = buffer.as_object().and_then(|b| JsArrayBuffer::from_object(b).ok()) else {
            return Ok(0);
        };
        let Some(mut data) = buffer.data_mut() else {
            return Ok(0);
        };
        let start = view_offset + offset.min(view_length);
        let end = (view_offset + view_length).min(data.len());
        let n = bytes.len().min(end.saturating_sub(start));
        data[start..start + n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }

    /// A `Buffer` of `bytes`, as `bun_jsc`'s `create_buffer`: a `Uint8Array` whose prototype is
    /// `Buffer.prototype`, where the realm has `Buffer`; a plain `Uint8Array` otherwise.
    pub fn create_buffer(global: &JSGlobalObject, bytes: &[u8]) -> JsResult<JSValue> {
        let array = JsUint8Array::from_iter(bytes.iter().copied(), global.cx());
        let array: JsObject = global.lift(array)?.into();
        as_buffer(global, &array)?;
        Ok(JSValue(array.into()))
    }

    /// Where the bytes of a typed array, a `DataView` or an `ArrayBuffer` are, and how many: what `bun:ffi` hands
    /// native code, which writes what JavaScript reads. `None` for anything else, for a detached buffer, and for a
    /// `SharedArrayBuffer` (Boa does not say where its bytes are).
    pub fn array_buffer_or_view_ptr(&self, global: &JSGlobalObject) -> JsResult<Option<(*mut u8, usize)>> {
        let Some(object) = self.0.as_object() else {
            return Ok(None);
        };
        let (buffer, offset, length) = if let Ok(array) = JsTypedArray::from_object(object.clone()) {
            let buffer = array.buffer(global.cx());
            let buffer = global.lift(buffer)?;
            let offset = array.byte_offset(global.cx());
            let offset = global.lift(offset)?;
            let length = array.byte_length(global.cx());
            (buffer, offset, Some(global.lift(length)?))
        } else if let Ok(view) = JsDataView::from_object(object.clone()) {
            let buffer = view.buffer(global.cx());
            let buffer = global.lift(buffer)?;
            let offset = view.byte_offset(global.cx());
            let offset = global.lift(offset)? as usize;
            let length = view.byte_length(global.cx());
            (buffer, offset, Some(global.lift(length)? as usize))
        } else {
            (JsValue::from(object), 0, None)
        };
        let Some(buffer) = buffer.as_object().and_then(|b| JsArrayBuffer::from_object(b).ok()) else {
            return Ok(None);
        };
        let Some(mut data) = buffer.data_mut() else {
            return Ok(None);
        };
        let length = length.unwrap_or(data.len());
        if offset + length > data.len() {
            return Ok(None);
        }
        Ok(Some((data[offset..].as_mut_ptr(), length)))
    }

    /// An `ArrayBuffer` over `len` bytes at `ptr`, which are not copied: `finalize` runs when it lets them go.
    ///
    /// # Safety
    /// The bytes are valid for reads and writes until `finalize` runs.
    pub unsafe fn create_external_array_buffer(
        global: &JSGlobalObject,
        ptr: *mut u8,
        len: usize,
        finalize: Option<Box<dyn FnOnce()>>,
    ) -> JsResult<JSValue> {
        // SAFETY: the contract of this function.
        let bytes = unsafe { ExternalBytes::new(ptr, len, finalize) };
        let buffer = JsArrayBuffer::from_external(bytes, global.cx());
        Ok(JSValue(global.lift(buffer)?.into()))
    }

    /// A `Buffer` over `len` bytes at `ptr`, as [`create_external_array_buffer`](Self::create_external_array_buffer).
    ///
    /// # Safety
    /// As `create_external_array_buffer`.
    pub unsafe fn create_external_buffer(
        global: &JSGlobalObject,
        ptr: *mut u8,
        len: usize,
        finalize: Option<Box<dyn FnOnce()>>,
    ) -> JsResult<JSValue> {
        // SAFETY: the contract of this function.
        let bytes = unsafe { ExternalBytes::new(ptr, len, finalize) };
        let buffer = JsArrayBuffer::from_external(bytes, global.cx());
        let buffer = global.lift(buffer)?;
        let array = JsUint8Array::from_array_buffer(buffer, global.cx());
        let array: JsObject = global.lift(array)?.into();
        as_buffer(global, &array)?;
        Ok(JSValue(array.into()))
    }
}

/// Gives `array` the prototype of the realm's `Buffer`, where it has one.
fn as_buffer(global: &JSGlobalObject, array: &JsObject) -> JsResult<()> {
    let buffer = global.to_js_value().get(global, "Buffer")?;
    if let Some(prototype) = buffer.map(|buffer| buffer.get(global, "prototype")).transpose()? {
        if let Some(prototype) = prototype.and_then(|prototype| prototype.0.as_object()) {
            array.set_prototype(Some(prototype));
        }
    }
    Ok(())
}

/// The bytes of an `ArrayBuffer` or a `SharedArrayBuffer`, `None` where it is detached.
pub(crate) fn buffer_bytes(
    global: &JSGlobalObject,
    buffer: Option<&JsObject>,
) -> JsResult<Option<Vec<u8>>> {
    let Some(buffer) = buffer else {
        return Ok(None);
    };
    if let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) {
        return Ok(buffer.data().map(|bytes| bytes.to_vec()));
    }
    let Ok(shared) = JsSharedArrayBuffer::from_object(buffer.clone()) else {
        return Ok(None);
    };
    let view = JsUint8Array::from_shared_array_buffer(shared, global.cx());
    let view = global.lift(view)?;
    Ok(Some(view.iter(global.cx()).collect()))
}
