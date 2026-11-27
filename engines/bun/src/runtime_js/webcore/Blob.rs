//! `Blob`, `File`, `Bun.file()` and `Bun.stdin`/`stdout`/`stderr`: src/runtime/webcore/Blob.rs on `bun_js`.
//!
//! A read or a write runs when it is asked for and its promise is settled by the time it is returned: Boa has no
//! thread pool and no I/O loop yet. `writer()` is a `FileSink` (FileSink.rs). What needs an API that Boa does not
//! have yet (`image()` needs `Bun.Image`, `s3://` needs S3) throws and says so.

use std::rc::Rc;

use boa_gc::{Finalize, Trace};
use boa_wintertc::store::HostSerialized;
use bun_core::strings;
use bun_http_types::MimeType::{self as mime_type, MimeType};
use bun_js::class::{self, Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, ErrorCode, JSFunction, JSGlobalObject, JSObject, JSValue, JsClass, JsResult};

use super::{form_data, object_url_registry};
use crate::engine;
use crate::sys_js::SystemError;

#[path = "blob/Store.rs"]
pub mod store;
#[path = "blob/write_file.rs"]
pub(crate) mod write_file;
#[path = "FileSink.rs"]
pub mod file_sink;

use store::Data;
pub use store::{INIT_TIMESTAMP, MAX_SIZE, PathOrFileDescriptor, SizeType, Store};

/// WHATWG File API §3.1: a Blob/File `type` is only used when every character is in the range U+0020 to U+007E;
/// otherwise it is treated as the empty string.
pub(crate) fn is_valid_blob_type(slice: &[u8]) -> bool {
    slice.iter().all(|&c| matches!(c, 0x20..=0x7E))
}

/// The type of a multipart part: printable ASCII and HTAB, so that a bare CR or LF never reaches a header.
pub(crate) fn is_valid_blob_type_or_tab(slice: &[u8]) -> bool {
    slice.iter().all(|&c| c == b'\t' || matches!(c, 0x20..=0x7E))
}

/// A Blob's `type`, as `bun_jsc::webcore_types::BlobContentType`.
#[derive(Clone)]
pub enum BlobContentType {
    Static(&'static [u8]),
    Owned(Rc<[u8]>),
    /// Script named a type that Bun sends with a charset: `type` is `given`, a `Content-Type` header is `header`.
    Named {
        given: &'static [u8],
        header: &'static [u8],
    },
}

impl Default for BlobContentType {
    fn default() -> Self {
        Self::Static(b"")
    }
}

impl BlobContentType {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Static(s) => s,
            Self::Owned(b) => b,
            Self::Named { header, .. } => header,
        }
    }

    /// What the `type` attribute answers.
    pub fn given(&self) -> &[u8] {
        match self {
            Self::Named { given, .. } => given,
            _ => self.as_slice(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }

    pub fn is_owned(&self) -> bool {
        matches!(self, Self::Owned(_))
    }

    pub fn from_mime(mime: &MimeType) -> Self {
        match &mime.value {
            std::borrow::Cow::Borrowed(s) => Self::Static(s),
            std::borrow::Cow::Owned(v) => Self::Owned(Rc::from(v.as_slice())),
        }
    }

    /// The type that script gave to `new Blob()`, `new File()` or `slice()`.
    pub fn from_script(slice: &[u8]) -> Self {
        Self::from_known_name(slice).unwrap_or_else(|| Self::from_lowercased(slice))
    }

    /// The `type` option of `Bun.file()` and of a write: a known name is its MIME type, charset included.
    pub fn from_type_option(slice: &[u8]) -> Self {
        match mime_type::by_name_static(slice) {
            Some(mime) => Self::from_mime(&mime),
            None => Self::from_lowercased(slice),
        }
    }

    fn from_known_name(slice: &[u8]) -> Option<Self> {
        let given = mime_type::interned_name(slice)?;
        Some(
            match mime_type::by_name_static(given).map(|mime| mime.value) {
                Some(std::borrow::Cow::Borrowed(header)) if header != given => {
                    Self::Named { given, header }
                }
                _ => Self::Static(given),
            },
        )
    }

    /// The type of a Blob that structured clone made again, from the `type` of the one it serialized.
    pub fn from_clone(given: &[u8]) -> Self {
        Self::from_known_name(given)
            .filter(|known| known.given() == given)
            .unwrap_or_else(|| Self::Owned(Rc::from(given)))
    }

    pub fn from_lowercased(slice: &[u8]) -> Self {
        Self::Owned(Rc::from(slice.to_ascii_lowercase()))
    }
}

/// A `Blob`'s payload: a window (`offset`, `size`) on a shared [`Store`].
#[derive(Clone, Trace, Finalize)]
pub struct Blob {
    #[unsafe_ignore_trace]
    pub(crate) store: Option<Rc<Store>>,
    /// [`MAX_SIZE`] until it is known.
    pub(crate) size: SizeType,
    pub(crate) offset: SizeType,
    #[unsafe_ignore_trace]
    pub(crate) content_type: BlobContentType,
    pub(crate) content_type_was_set: bool,
    /// Made by the `File` constructor: `instanceof File` is true of it.
    pub(crate) is_jsdom_file: bool,
    pub(crate) last_modified: f64,
    /// `name`, a string, where it was given or asked for.
    pub(crate) name: Option<JSValue>,
}

thread_local! {
    /// `rare_data().stdio_store()`: one store per standard descriptor, which `Bun.stdin` and `Bun.file(0)` share.
    static STDIO: [Rc<Store>; 3] = [0, 1, 2].map(|fd| {
        Store::init_file(PathOrFileDescriptor::Fd(bun_sys::Fd::from_uv(fd)), None)
    });
}

impl Blob {
    pub fn init_empty() -> Blob {
        Blob {
            store: None,
            size: 0,
            offset: 0,
            content_type: BlobContentType::default(),
            content_type_was_set: false,
            is_jsdom_file: false,
            last_modified: 0.0,
            name: None,
        }
    }

    /// A Blob of `bytes`, without a store if there are none.
    pub fn init(bytes: Vec<u8>) -> Blob {
        let size = bytes.len() as SizeType;
        let mut blob = Blob::init_empty();
        blob.store = (size > 0).then(|| Store::init(bytes));
        blob.size = size;
        blob
    }

    pub fn init_with_store(store: Rc<Store>) -> Blob {
        let content_type = match &store.data {
            Data::File(file) => BlobContentType::from_mime(&file.mime_type),
            Data::Bytes(_) => BlobContentType::default(),
        };
        let mut blob = Blob::init_empty();
        blob.size = store.size();
        blob.store = Some(store);
        blob.content_type = content_type;
        blob
    }

    /// `Bun.file(pathOrFd)`: a Blob of a file, the shared one of a standard descriptor.
    pub fn find_or_create_file_from_path(path_or_fd: PathOrFileDescriptor) -> Blob {
        if let PathOrFileDescriptor::Fd(fd) = &path_or_fd
            && let Ok(index @ 0..=2) = usize::try_from(fd.uv())
        {
            return Blob::init_with_store(STDIO.with(|stdio| stdio[index].clone()));
        }
        Blob::init_with_store(Store::init_file(path_or_fd, None))
    }

    pub(crate) fn file(&self) -> Option<&store::File> {
        self.store.as_deref().and_then(Store::as_file)
    }

    /// The bytes in memory this Blob views; empty for a file.
    pub fn shared_view(&self) -> &[u8] {
        if self.size == 0 {
            return &[];
        }
        match self.store.as_deref().map(|store| &store.data) {
            Some(Data::Bytes(bytes)) => {
                let offset = (self.offset as usize).min(bytes.len());
                let len = (bytes.len() - offset).min(self.size as usize);
                &bytes[offset..offset + len]
            }
            _ => &[],
        }
    }

    /// What the Blob holds: its bytes in memory, or what a read of its file gives.
    pub fn read_bytes(&self) -> Result<Vec<u8>, SystemError> {
        match self.file() {
            Some(file) => file.read(self.offset, self.size),
            None => Ok(self.shared_view().to_vec()),
        }
    }

    fn resolve_size(&mut self) {
        let Some(store) = self.store.clone() else {
            self.size = 0;
            return;
        };
        match &store.data {
            Data::Bytes(bytes) => {
                let store_size = bytes.len() as SizeType;
                self.offset = store_size.min(self.offset);
                self.size = window_size(self.size, store_size - self.offset);
            }
            Data::File(file) => {
                if file.seekable.get().is_none() {
                    file.resolve_stat();
                }
                if file.seekable.get().is_some() && file.max_size.get() != MAX_SIZE {
                    let store_size = file.max_size.get();
                    self.offset = store_size.min(self.offset);
                    self.size = window_size(self.size, store_size - self.offset);
                    return;
                }
                // A pipe's size is unknown, and stays so: a stream of it is not empty.
                if file.seekable.get() == Some(false) {
                    return;
                }
                self.size = 0;
            }
        }
    }

    /// `size`: `Infinity` for what has no end it can know (a pipe, a terminal).
    pub fn get_size(&mut self) -> f64 {
        if self.size == MAX_SIZE {
            self.resolve_size();
            if self.size == MAX_SIZE && self.store.is_some() {
                return f64::INFINITY;
            }
            if self.size == 0
                && let Some(file) = self.file()
                && !file.seekable.get().unwrap_or(true)
                && file.max_size.get() == MAX_SIZE
            {
                return f64::INFINITY;
            }
        }
        self.size as f64
    }

    pub fn get_last_modified(&self) -> f64 {
        if let Some(file) = self.file() {
            if file.last_modified.get() == INIT_TIMESTAMP {
                file.resolve_stat();
            }
            return file.last_modified.get() as f64;
        }
        if self.is_jsdom_file {
            return self.last_modified;
        }
        INIT_TIMESTAMP as f64
    }

    /// `exists()`: memory always does; a file does if it is a regular one or a pipe.
    pub fn exists_sync(&mut self) -> bool {
        if self.size == MAX_SIZE {
            self.resolve_size();
        } else if let Some(file) = self.file()
            && file.seekable.get().is_none()
        {
            // A failed stat is not an answer to keep: the file may exist by now.
            file.resolve_stat();
        }
        match self.store.as_deref().map(|store| &store.data) {
            None | Some(Data::Bytes(_)) => true,
            Some(Data::File(file)) => {
                bun_sys::S::ISREG(file.mode.get()) || bun_sys::S::ISFIFO(file.mode.get())
            }
        }
    }

    /// `slice()` once its arguments are numbers: the window `relative_start..relative_end` of this one.
    fn get_slice_from(
        &self,
        relative_start: i64,
        relative_end: i64,
        content_type: BlobContentType,
    ) -> Blob {
        let mut blob = self.clone();
        blob.offset = self
            .offset
            .saturating_add(relative_start.max(0) as SizeType);
        blob.size = relative_end.saturating_sub(relative_start).max(0) as SizeType;
        let content_type_was_allocated = content_type.is_owned() && !content_type.is_empty();
        // A slice is of the type of its Blob, unless that is one script made up.
        if content_type.is_empty()
            && !self.content_type.is_owned()
            && !self.content_type.is_empty()
        {
            blob.content_type = self.content_type.clone();
        } else {
            blob.content_type = content_type;
        }
        blob.content_type_was_set = self.content_type_was_set || content_type_was_allocated;
        blob
    }

    /// `name`: the name given, else the path of the file.
    fn get_name(&mut self) -> JSValue {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let Some(path) = self.file().and_then(|file| file.pathlike.path()) else {
            return JSValue::UNDEFINED;
        };
        let name = JSValue::from_utf8(path);
        self.name = Some(name.clone());
        name
    }

    /// `Blob::get`: a Blob of what script gave, the parts of `new Blob(parts)` or the data of `Bun.write`.
    /// `require_array`: anything but an array or a buffer throws, as `new Blob("ok")` does.
    pub fn get(global: &JSGlobalObject, arg: &JSValue, require_array: bool) -> JsResult<Blob> {
        if arg.is_undefined_or_null() {
            return Ok(Blob::init_empty());
        }
        let mut top_value = arg.clone();
        if arg.is_array() {
            let len = arg.get_length(global)?;
            if len == 0 {
                return Ok(Blob::init_empty());
            }
            if len == 1 {
                top_value = arg.get_index(global, 0)?;
            }
        }
        let fail_if_not_typed_array_like = require_array && !arg.is_array();

        // One part: no joining.
        if let Some(bytes) = engine::buffer_source_bytes(global, &top_value) {
            return Ok(Blob::init(bytes));
        }
        if !fail_if_not_typed_array_like {
            if top_value.is_string() || engine::string_object_value(&top_value).is_some() {
                return Ok(Blob::init(top_value.to_utf8(global)?.into_bytes()));
            }
            if let Some(blob) = top_value.as_class_ref::<Blob>() {
                return Ok(blob.borrow(global)?.clone());
            }
        } else {
            return Err(ErrorCode::INVALID_ARG_TYPE
                .throw(global, format_args!("new Blob() expects an Array")));
        }

        let mut joined = Vec::new();
        if !arg.is_array() {
            joined.extend_from_slice(arg.to_utf8(global)?.as_bytes());
            return Ok(Blob::init(joined));
        }
        // The length is read once, and each part by its index, as `JSArrayIterator`: a hole is what the prototype
        // chain has there.
        let len = arg.get_length(global)?;
        for index in 0..len {
            let item = arg.get_index(global, index as u32)?;
            if item.is_undefined_or_null() {
                continue;
            }
            if let Some(bytes) = engine::buffer_source_bytes(global, &item) {
                joined.extend_from_slice(&bytes);
            } else if let Some(blob) = item.as_class_ref::<Blob>() {
                joined.extend_from_slice(blob.borrow(global)?.shared_view());
            } else {
                joined.extend_from_slice(item.to_utf8(global)?.as_bytes());
            }
        }
        Ok(Blob::init(joined))
    }

    /// `new File(bits, name, options)`.
    fn construct_file(
        global: &JSGlobalObject,
        bits: &JSValue,
        name: &JSValue,
        options: &JSValue,
    ) -> JsResult<Blob> {
        let name = JSValue::from_utf16(&name.to_utf16(global)?);
        let mut blob = Blob::get(global, bits, true)?;
        blob.name = Some(name);
        let mut set_last_modified = false;
        if options.is_object() {
            if let Some(content_type) = options.get(global, "type")?
                && content_type.is_string()
            {
                let content_type = content_type.to_utf8(global)?;
                if is_valid_blob_type(content_type.as_bytes()) {
                    blob.content_type_was_set = true;
                    blob.content_type = BlobContentType::from_script(content_type.as_bytes());
                }
            }
            // A WebIDL dictionary member: only `undefined` is the default; `null` is ToNumber's 0, NaN is 0.
            if let Some(last_modified) = options.get(global, "lastModified")? {
                set_last_modified = true;
                let n = last_modified.to_number(global)?;
                blob.last_modified = if n.is_nan() { 0.0 } else { n };
            }
        }
        if !set_last_modified {
            blob.last_modified = bun_core::time::milli_timestamp() as f64;
        }
        if blob.content_type.is_empty() {
            blob.content_type = BlobContentType::default();
            blob.content_type_was_set = false;
        }
        blob.is_jsdom_file = true;
        Ok(blob)
    }

    pub fn to_js(self, global: &JSGlobalObject) -> JsResult<JSValue> {
        class::to_js(global, self)
    }
}

impl Blob {
    /// `write_format` without colors: what `Bun.inspect` and `console.log` print of a Blob.
    pub fn write_format(&self, out: &mut String) {
        use core::fmt::Write;
        let size_of = |out: &mut String, size: usize| {
            out.push_str(if self.is_jsdom_file { "File" } else { "Blob" });
            let _ = write!(out, " ({})", bun_core::fmt::size(size, Default::default()));
        };
        let is_bytes = match self.store.as_deref().map(|store| &store.data) {
            None => {
                size_of(out, 0);
                false
            }
            Some(Data::File(file)) => {
                out.push_str("FileRef");
                match &file.pathlike {
                    PathOrFileDescriptor::Path(path) => {
                        let _ = write!(out, " (\"{}\")", String::from_utf8_lossy(path));
                    }
                    PathOrFileDescriptor::Fd(fd) => {
                        let _ = write!(out, " (fd: {})", fd.native());
                    }
                }
                false
            }
            Some(Data::Bytes(_)) => {
                size_of(out, self.size as usize);
                true
            }
        };
        let name = self.name.as_ref().and_then(JSValue::as_string);
        let show_name = (self.is_jsdom_file && name.is_some())
            || (name.as_ref().is_some_and(|name| !name.is_empty()) && is_bytes);
        let content_type = self.content_type.given();
        let last_modified = self.last_modified != 0.0;
        if content_type.is_empty() && self.offset == 0 && !show_name && !last_modified {
            return;
        }
        out.push_str(" {\n");
        if show_name {
            let _ = write!(out, "  name: \"{}\"", name.unwrap_or_default());
            if !content_type.is_empty() || self.offset > 0 || last_modified {
                out.push(',');
            }
            out.push('\n');
        }
        if !content_type.is_empty() {
            let _ = write!(out, "  type: \"{}\"", String::from_utf8_lossy(content_type));
            if self.offset > 0 || last_modified {
                out.push(',');
            }
            out.push('\n');
        }
        if self.offset > 0 {
            let _ = writeln!(out, "  offset: {}", self.offset);
            if last_modified {
                out.push(',');
            }
            out.push('\n');
        }
        if last_modified {
            let _ = writeln!(out, "  lastModified: {}", self.last_modified);
        }
        out.push('}');
    }
}

/// `[util.inspect.custom]()`: Bun.inspect on Boa is node's util.inspect, which prints what this returns.
fn inspect_custom(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let mut out = String::new();
    match frame.this().as_class_ref::<Blob>() {
        Some(blob) => blob.borrow(global)?.write_format(&mut out),
        None => Blob::init_empty().write_format(&mut out),
    }
    Ok(JSValue::from_utf8(out.as_bytes()))
}

/// What structured clone keeps of a Blob, as `on_structured_clone_serialize` writes it: the bytes it views, or the
/// file and the end of its window.
#[derive(Debug)]
enum SerializedData {
    Empty,
    Bytes(Vec<u8>),
    File {
        pathlike: PathOrFileDescriptor,
        size: SizeType,
    },
}

#[derive(Debug)]
struct SerializedBlob {
    data: SerializedData,
    offset: SizeType,
    content_type: Vec<u8>,
    content_type_was_set: bool,
    is_jsdom_file: bool,
    last_modified: f64,
    name: Option<String>,
}

impl SerializedBlob {
    fn of(blob: &Blob) -> SerializedBlob {
        let (data, offset) = match blob.store.as_deref().map(|store| &store.data) {
            None => (SerializedData::Empty, blob.offset),
            Some(Data::Bytes(_)) => (SerializedData::Bytes(blob.shared_view().to_vec()), 0),
            Some(Data::File(file)) => (
                SerializedData::File {
                    pathlike: file.pathlike.clone(),
                    size: blob.size,
                },
                blob.offset,
            ),
        };
        SerializedBlob {
            data,
            offset,
            content_type: blob.content_type.given().to_vec(),
            content_type_was_set: blob.content_type_was_set,
            is_jsdom_file: blob.is_jsdom_file,
            last_modified: blob.last_modified,
            name: blob
                .is_jsdom_file
                .then(|| blob.name.as_ref().and_then(JSValue::as_string).unwrap_or_default()),
        }
    }

    /// `on_structured_clone_deserialize`.
    fn blob(&self) -> Blob {
        let mut file_size = None;
        let mut blob = match &self.data {
            SerializedData::Empty => Blob::init_empty(),
            SerializedData::Bytes(bytes) => Blob::init(bytes.clone()),
            SerializedData::File { pathlike, size } => {
                file_size = Some(*size);
                Blob::find_or_create_file_from_path(pathlike.clone())
            }
        };
        blob.is_jsdom_file = self.is_jsdom_file;
        blob.last_modified = self.last_modified;
        if let Some(name) = &self.name {
            blob.name = Some(JSValue::from_utf8(name.as_bytes()));
        }
        blob.offset = self.offset;
        if let Some(size) = file_size.filter(|&size| size < MAX_SIZE) {
            blob.size = size;
        }
        match blob.store.as_deref().map(Store::size) {
            Some(store_size) if store_size != MAX_SIZE => {
                blob.offset = blob.offset.min(store_size);
                blob.size = blob.size.min(store_size - blob.offset);
            }
            Some(_) => {}
            None => blob.offset = 0,
        }
        if !self.content_type.is_empty() {
            blob.content_type = BlobContentType::from_clone(&self.content_type);
            blob.content_type_was_set = self.content_type_was_set;
        }
        blob
    }
}

impl HostSerialized for SerializedBlob {
    fn deserialize(&self, context: &mut boa_engine::Context) -> boa_engine::JsResult<boa_engine::JsValue> {
        let global = JSGlobalObject::from_context(context);
        self.blob()
            .to_js(global)
            .map(JSValue::into_engine)
            .map_err(|error| global.into_engine_error(error))
    }
}

/// The serialization steps of a Blob or a File.
pub(crate) fn serialize_host(
    object: &boa_engine::JsObject,
    context: &mut boa_engine::Context,
) -> boa_engine::JsResult<Option<Box<dyn HostSerialized>>> {
    let global = JSGlobalObject::from_context(context);
    let Some(blob) = JSValue::from_engine(object.clone().into()).as_class_ref::<Blob>() else {
        return Ok(None);
    };
    let serialized = match blob.borrow(global) {
        Ok(blob) => SerializedBlob::of(&blob),
        Err(error) => return Err(global.into_engine_error(error)),
    };
    Ok(Some(Box::new(serialized)))
}

/// Only an unknown size ([`MAX_SIZE`]) becomes the rest of the store; a known one (a slice's) is clamped to it.
fn window_size(current: SizeType, available: SizeType) -> SizeType {
    if current == MAX_SIZE {
        available
    } else {
        current.min(available)
    }
}

/// `SystemError` as the exception, for a body that `engine::wrap` makes a rejection of.
pub(crate) fn throw_system_error(global: &JSGlobalObject, error: &SystemError) -> bun_js::JsError {
    global.throw_value(error.to_error_instance(global))
}

/// Text as `Blob.text()` decodes it: a UTF-16LE BOM is UTF-16; else UTF-8 without its BOM, invalid sequences as
/// U+FFFD.
fn decode_text(bytes: &[u8]) -> JSValue {
    let (bom, buf) = strings::BOM::detect_and_split(bytes);
    if bom == Some(strings::BOM::Utf16Le) {
        return engine::string_from_utf16le(buf);
    }
    JSValue::from_utf8(buf)
}

fn json_from_bytes(global: &JSGlobalObject, bytes: &[u8]) -> JsResult<JSValue> {
    let (bom, buf) = strings::BOM::detect_and_split(bytes);
    if buf.is_empty() {
        return Err(global.throw_value(
            global.create_syntax_error_instance(format_args!("Unexpected end of JSON input")),
        ));
    }
    let text = match bom {
        Some(strings::BOM::Utf16Le) => engine::string_from_utf16le(buf),
        _ => JSValue::from_utf8(buf),
    };
    engine::parse_json(global, text)
}

/// A promise of what `convert` makes of the Blob's bytes, or of the error the read gave.
fn read_then(
    this: &ClassRef<Blob>,
    global: &JSGlobalObject,
    convert: fn(&JSGlobalObject, Vec<u8>) -> JsResult<JSValue>,
) -> JsResult<JSValue> {
    // A file is read on the work pool, where the realm runs on bun_runtime_js's event loop.
    let blob = this.borrow(global)?.clone();
    if let Some(file) = blob.file()
        && let Some(event_loop) = crate::cx(global).downcast_job_executor::<crate::EventLoop>()
    {
        let pathlike = file.pathlike.clone();
        let (offset, size, kept) = (blob.offset, blob.size, blob.store.clone());
        let promise = bun_js::JSPromise::create(global);
        let value = promise.promise();
        event_loop.run_on_work_pool(
            move || store::read(&pathlike, offset, size),
            move |context, result| {
                let global = JSGlobalObject::from_context(context);
                let settled = match result {
                    Ok(read) => {
                        if let Some(file) = kept.as_deref().and_then(Store::as_file) {
                            file.last_modified.set(read.last_modified);
                        }
                        convert(global, read.bytes).map_err(|error| global.take_exception(error))
                    }
                    Err(error) => Err(error.to_error_instance(global)),
                };
                let result = match settled {
                    Ok(value) => promise.resolve(global, value),
                    Err(error) => promise.reject(global, error),
                };
                crate::lift(global, result)
            },
        );
        return Ok(value);
    }
    engine::wrap(global, || {
        let bytes = this.borrow(global)?.read_bytes();
        match bytes {
            Ok(bytes) => convert(global, bytes),
            Err(error) => Err(throw_system_error(global, &error)),
        }
    })
}

/// `validate_writable_blob`.
pub(crate) fn validate_writable_blob(global: &JSGlobalObject, blob: &Blob) -> JsResult<()> {
    let Some(store) = &blob.store else {
        return Err(global.throw(format_args!("Cannot write to a detached Blob")));
    };
    if matches!(store.data, Data::Bytes(_)) {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("Cannot write to a Blob backed by bytes, which are always read-only"),
        ));
    }
    Ok(())
}

fn not_implemented(global: &JSGlobalObject, what: &str) -> bun_js::JsError {
    global.throw(format_args!("{what} is not implemented on Boa yet"))
}

/// `blob.unlink()` / `blob.delete()`.
fn unlink(this: &ClassRef<Blob>, global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let blob = this.borrow(global)?.clone();
    validate_writable_blob(global, &blob)?;
    let file = blob.file().expect("validate_writable_blob: a file");
    match &file.pathlike {
        PathOrFileDescriptor::Path(path) => {
            match bun_sys::unlink(&bun_paths::Path::from(path.as_slice())) {
                Ok(()) => engine::resolved(global, JSValue::UNDEFINED),
                Err(err) => {
                    let error = SystemError::from(err.with_path(path));
                    engine::rejected(global, error.to_error_instance(global))
                }
            }
        }
        PathOrFileDescriptor::Fd(_) => engine::resolved(
            global,
            ErrorCode::INVALID_ARG_TYPE.fmt(
                global,
                format_args!("Is not possible to unlink a file descriptor"),
            ),
        ),
    }
}

/// `blob.stat()`: node:fs's `Stats` of the file, `undefined` for memory.
fn stat(this: &ClassRef<Blob>, global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let blob = this.borrow(global)?.clone();
    let Some(file) = blob.file() else {
        return Ok(JSValue::UNDEFINED);
    };
    engine::wrap(global, || {
        let stat = match &file.pathlike {
            PathOrFileDescriptor::Path(path) => {
                bun_sys::stat(&bun_paths::Path::from(path.as_slice()))
                    .map_err(|err| err.with_path(path))
            }
            PathOrFileDescriptor::Fd(fd) => bun_sys::fstat(*fd),
        };
        match stat {
            Ok(stat) => stats_to_js(global, &bun_sys::PosixStat::init(&stat)),
            Err(err) => Err(throw_system_error(global, &SystemError::from(err))),
        }
    })
}

/// node:fs's `Stats`: its numbers, its dates and its `is*()` methods.
fn stats_to_js(global: &JSGlobalObject, stat: &bun_sys::PosixStat) -> JsResult<JSValue> {
    let object = JSObject::create(global);
    let ms = |time: bun_core::util::Timespec| time.sec as f64 * 1000.0 + time.nsec as f64 / 1e6;
    for (key, value) in [
        ("dev", stat.dev),
        ("ino", stat.ino),
        ("mode", stat.mode),
        ("nlink", stat.nlink),
        ("uid", stat.uid),
        ("gid", stat.gid),
        ("rdev", stat.rdev),
        ("size", stat.size),
        ("blksize", stat.blksize),
        ("blocks", stat.blocks),
    ] {
        object.put(global, key, JSValue::js_number_from_uint64(value));
    }
    let times = [
        ("atime", stat.atim),
        ("mtime", stat.mtim),
        ("ctime", stat.ctim),
        ("birthtime", stat.birthtim),
    ];
    for (key, time) in times {
        object.put(global, format!("{key}Ms"), JSValue::js_number(ms(time)));
    }
    for (key, time) in times {
        object.put(global, key, engine::date(global, ms(time))?);
    }
    let methods: [(&str, bun_js::JSHostFn); 7] = [
        ("isBlockDevice", |g, f| mode_is(g, f, bun_sys::S::IFBLK)),
        ("isCharacterDevice", |g, f| mode_is(g, f, bun_sys::S::IFCHR)),
        ("isDirectory", |g, f| mode_is(g, f, bun_sys::S::IFDIR)),
        ("isFIFO", |g, f| mode_is(g, f, bun_sys::S::IFIFO)),
        ("isFile", |g, f| mode_is(g, f, bun_sys::S::IFREG)),
        ("isSocket", |g, f| mode_is(g, f, bun_sys::S::IFSOCK)),
        ("isSymbolicLink", |g, f| mode_is(g, f, bun_sys::S::IFLNK)),
    ];
    for (name, method) in methods {
        object.put_non_enumerable(global, name, JSFunction::create(global, name, method, 0))?;
    }
    Ok(object.to_js())
}

fn mode_is(global: &JSGlobalObject, frame: &CallFrame<'_>, kind: bun_sys::Mode) -> JsResult<JSValue> {
    let mode = match frame.this().get(global, "mode")? {
        Some(mode) => mode.to_number(global)? as u64 as bun_sys::Mode,
        None => 0,
    };
    Ok(JSValue::js_boolean(mode & bun_sys::S::IFMT == kind))
}

/// `blob.slice(start, end, contentType)`: <https://w3c.github.io/FileAPI/#slice-method-algo>.
fn slice(this: &ClassRef<Blob>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let [start, end, content_type] = frame.arguments_as_array::<3>();
    let size = this.borrow(global)?.size;
    if size == 0 {
        return Blob::init_empty().to_js(global);
    }
    let size = i64::try_from(size).unwrap_or(i64::MAX);
    // A string in place of `start` or `end` is the content type.
    let (start, end, content_type) = if start.is_string() {
        (JSValue::UNDEFINED, JSValue::UNDEFINED, start)
    } else if end.is_string() {
        (start, JSValue::UNDEFINED, end)
    } else {
        (start, end, content_type)
    };
    let relative = |value: &JSValue, default: i64| match value.get_number() {
        Some(n) => {
            let n = n as i64;
            if n < 0 {
                n.saturating_add(size).max(0)
            } else {
                n.min(size)
            }
        }
        None => default,
    };
    let (relative_start, relative_end) = (relative(&start, 0), relative(&end, size));
    let mut blob_content_type = BlobContentType::default();
    if content_type.is_string() {
        let content_type = content_type.to_utf8(global)?;
        if is_valid_blob_type(content_type.as_bytes()) {
            blob_content_type = BlobContentType::from_script(content_type.as_bytes());
        }
    }
    let blob = this
        .borrow(global)?
        .get_slice_from(relative_start, relative_end, blob_content_type);
    blob.to_js(global)
}

/// `blob.write(data, options)`.
fn blob_write(
    this: &ClassRef<Blob>,
    global: &JSGlobalObject,
    frame: &CallFrame<'_>,
) -> JsResult<JSValue> {
    let [data, options] = frame.arguments_as_array::<2>();
    let blob = this.borrow(global)?.clone();
    validate_writable_blob(global, &blob)?;
    if data.is_undefined_or_null() {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("blob.write(pathOrFdOrBlob, blob) expects a Blob-y thing to write"),
        ));
    }
    let mut mkdirp_if_not_exists = None;
    if !options.is_undefined() {
        if options.is_object() {
            if let Some(create_directory) = options.get(global, "createPath")?
                && create_directory.is_truthy()
            {
                if !create_directory.is_boolean() {
                    return Err(ErrorCode::INVALID_ARG_TYPE.throw(
                        global,
                        format_args!("Expected options.createPath to be a boolean for 'write'."),
                    ));
                }
                mkdirp_if_not_exists = Some(create_directory.to_boolean());
            }
            if let Some(content_type) = options.get(global, "type")?
                && content_type.is_truthy()
            {
                if !content_type.is_string() {
                    return Err(ErrorCode::INVALID_ARG_TYPE.throw(
                        global,
                        format_args!("Expected options.type to be a string for 'write'."),
                    ));
                }
                let content_type = content_type.to_utf8(global)?;
                if is_valid_blob_type(content_type.as_bytes()) {
                    let mut blob = this.borrow_mut(global)?;
                    blob.content_type_was_set = true;
                    blob.content_type = BlobContentType::from_type_option(content_type.as_bytes());
                }
            }
        } else if !options.is_null() {
            return Err(ErrorCode::INVALID_ARG_TYPE.throw(
                global,
                format_args!("Expected options to be a object for 'write'."),
            ));
        }
    }
    write_file::write_file_internal(
        global,
        write_file::Destination::Blob(blob),
        &data,
        write_file::WriteFileOptions {
            mkdirp_if_not_exists,
            mode: None,
        },
    )
}

impl JsClass for Blob {
    const NAME: &'static str = "Blob";
    const LENGTH: usize = 0;

    /// `new Blob(parts, options)`.
    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Blob> {
        let [parts, options] = frame.arguments_as_array::<2>();
        if parts.is_undefined() && options.is_undefined() {
            return Ok(Blob::init_empty());
        }
        let mut blob = Blob::get(global, &parts, true)?;
        if options.is_object()
            && let Some(content_type) = options.get(global, "type")?
            && content_type.is_string()
        {
            let content_type = content_type.to_utf8(global)?;
            if is_valid_blob_type(content_type.as_bytes()) {
                blob.content_type_was_set = true;
                blob.content_type = BlobContentType::from_script(content_type.as_bytes());
            }
        }
        if blob.content_type.is_empty() {
            blob.content_type = BlobContentType::default();
            blob.content_type_was_set = false;
        }
        Ok(blob)
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.method("arrayBuffer", 0, attributes, |this, global, _| {
            read_then(this, global, |global, bytes| engine::array_buffer(global, &bytes))
        })?;
        class.method("bytes", 0, attributes, |this, global, _| {
            read_then(this, global, |global, bytes| engine::uint8_array(global, &bytes))
        })?;
        class.method("delete", 0, attributes, unlink)?;
        class.method("exists", 0, attributes, |this, global, _| {
            let exists = this.borrow_mut(global)?.exists_sync();
            engine::resolved(global, JSValue::js_boolean(exists))
        })?;
        class.method("formData", 0, attributes, |this, global, _| {
            engine::wrap(global, || {
                let blob = this.borrow(global)?.clone();
                if blob.file().is_none() && blob.shared_view().is_empty() {
                    return class::to_js(global, form_data::FormData::default());
                }
                let bytes = blob
                    .read_bytes()
                    .map_err(|error| throw_system_error(global, &error))?;
                form_data::of_blob_bytes(global, blob.content_type.as_slice(), &bytes)
            })
        })?;
        class.method("image", 0, attributes, |_, global, _| {
            Err(not_implemented(global, "Blob.image()"))
        })?;
        class.method("json", 0, attributes, |this, global, _| {
            read_then(this, global, |global, bytes| json_from_bytes(global, &bytes))
        })?;
        class.getter("lastModified", attributes, |this, global| {
            Ok(JSValue::js_number(this.borrow(global)?.get_last_modified()))
        })?;
        class.accessor(
            "name",
            attributes,
            |this, global| Ok(this.borrow_mut(global)?.get_name()),
            |this, global, value| {
                // Without a name to start with, `undefined` and `null` are names too; anything else but a string
                // is not.
                if value.is_undefined_or_null() || value.is_string() {
                    this.borrow_mut(global)?.name = Some(value);
                }
                Ok(())
            },
        )?;
        class.getter("size", attributes, |this, global| {
            Ok(JSValue::js_number(this.borrow_mut(global)?.get_size()))
        })?;
        class.method("slice", 2, attributes, slice)?;
        class.method("stat", 0, attributes, stat)?;
        class.method("stream", 1, attributes, |this, global, _| {
            // A descriptor (Bun.stdin) is read as a reader asks; anything else now, as the stream's one chunk.
            let fd = this.borrow(global)?.file().and_then(|file| match &file.pathlike {
                PathOrFileDescriptor::Fd(fd) => Some(*fd),
                PathOrFileDescriptor::Path(_) => None,
            });
            if let Some(fd) = fd {
                return file_sink::stream_of_fd(global, fd);
            }
            let bytes = this
                .borrow(global)?
                .read_bytes()
                .map_err(|error| throw_system_error(global, &error))?;
            let chunks = match bytes.is_empty() {
                true => Vec::new(),
                false => vec![engine::uint8_array(global, &bytes)?],
            };
            crate::web::streams::readable_stream::of_chunks(global, &chunks)
        })?;
        class.method("text", 0, attributes, |this, global, _| {
            read_then(this, global, |_, bytes| Ok(decode_text(&bytes)))
        })?;
        class.getter("type", attributes, |this, global| {
            Ok(JSValue::from_utf8(this.borrow(global)?.content_type.given()))
        })?;
        class.method("unlink", 0, attributes, unlink)?;
        class.method("write", 2, attributes, blob_write)?;
        class.method("writer", 1, attributes, |this, global, frame| {
            let blob = this.borrow(global)?.clone();
            super::file_sink::writer(global, &blob, &frame.argument(0))
        })?;
        Ok(())
    }
}

/// The `File` constructor: its instances are Blobs (`File.prototype` is `Blob.prototype`, as in JSDOMFile.cpp),
/// and `instanceof File` asks whether the `File` constructor made one.
fn file_constructor(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let new_target = frame.this().clone();
    if !new_target.is_callable() {
        return Err(global.throw_type_error(format_args!(
            "Class constructor File cannot be invoked without 'new'"
        )));
    }
    if frame.arguments_count() < 2 {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("new File(bits, name) expects at least 2 arguments"),
        ));
    }
    let [bits, name, options] = frame.arguments_as_array::<3>();
    let prototype = new_target.get(global, "prototype")?;
    let blob = Blob::construct_file(global, &bits, &name, &options)?;
    let instance = blob.to_js(global)?;
    if let Some(prototype) = prototype.filter(JSValue::is_object) {
        engine::set_prototype(&instance, &prototype);
    }
    Ok(instance)
}

fn file_has_instance(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let is_file = match frame.argument(0).as_class_ref::<Blob>() {
        Some(blob) => blob.borrow(global)?.is_jsdom_file,
        None => false,
    };
    Ok(JSValue::js_boolean(is_file))
}

/// `PathOrFileDescriptor::from_js`: a descriptor, a path string, a `Uint8Array`/`DataView`/`ArrayBuffer` of one,
/// or a `file:` URL. `None` for anything else.
pub(crate) fn path_or_fd_from_js(
    global: &JSGlobalObject,
    value: &JSValue,
) -> JsResult<Option<PathOrFileDescriptor>> {
    if let Some(number) = value.get_number() {
        return fd_from_number(global, number).map(Some);
    }
    let path = if value.is_string() || engine::string_object_value(value).is_some() {
        value.to_utf8(global)?.into_bytes()
    } else if let Some(bytes) = engine::path_buffer_bytes(global, value) {
        if bytes.is_empty() {
            return Err(ErrorCode::INVALID_ARG_TYPE
                .throw(global, format_args!("Invalid path buffer: can't be empty")));
        }
        bytes
    } else if engine::is_instance_of_global(global, value, "URL")? {
        path_from_file_url(global, value)?
    } else {
        return Ok(None);
    };
    if strings::index_of_char(&path, 0).is_some() {
        return Err(ErrorCode::INVALID_ARG_VALUE.throw(
            global,
            format_args!(
                "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received {}",
                bun_core::fmt::quote(&path)
            ),
        ));
    }
    if path.len() >= bun_core::MAX_PATH_BYTES {
        let mut error = SystemError::from(
            bun_sys::Error::from_code(bun_sys::E::ENAMETOOLONG, bun_sys::Tag::open)
                .with_path(&path),
        );
        error.syscall = None;
        return Err(throw_system_error(global, &error));
    }
    Ok(Some(PathOrFileDescriptor::Path(path)))
}

/// `Fd::from_js_validated`: an integer from 0 to 2^31 - 1.
fn fd_from_number(global: &JSGlobalObject, number: f64) -> JsResult<PathOrFileDescriptor> {
    let out_of_range = |msg: &[u8]| -> JsResult<bun_js::JsError> {
        let options = bun_core::fmt::OutOfRangeOptions {
            field_name: b"fd",
            msg,
            ..Default::default()
        };
        // As JavaScript prints the number.
        let received = JSValue::js_number(number).to_utf8(global)?;
        Ok(ErrorCode::OUT_OF_RANGE.throw(
            global,
            format_args!(
                "{}",
                bun_core::fmt::out_of_range(received.as_bytes(), options)
            ),
        ))
    };
    if number % 1.0 != 0.0 {
        return Err(out_of_range(b"an integer")?);
    }
    if !(0.0..=f64::from(i32::MAX)).contains(&number) {
        let range = format!(">= 0 && <= {}", i32::MAX);
        return Err(out_of_range(range.as_bytes())?);
    }
    Ok(PathOrFileDescriptor::Fd(bun_sys::Fd::from_uv(number as i32)))
}

/// The path of a `file:` URL, as `DOMURL::fileSystemPath`.
fn path_from_file_url(global: &JSGlobalObject, url: &JSValue) -> JsResult<Vec<u8>> {
    let field = |name: &str| -> JsResult<String> {
        match url.get(global, name)? {
            Some(value) => value.to_utf8(global),
            None => Ok(String::new()),
        }
    };
    const MESSAGE: &str = "URL must be a non-empty \"file:\" path";
    if field("protocol")? != "file:" {
        return Err(ErrorCode::INVALID_URL_SCHEME.throw(global, format_args!("{MESSAGE}")));
    }
    let hostname = field("hostname")?;
    if !hostname.is_empty() && hostname != "localhost" {
        return Err(ErrorCode::INVALID_FILE_URL_HOST.throw(global, format_args!("{MESSAGE}")));
    }
    let pathname = field("pathname")?;
    let encoded = pathname.as_bytes();
    let mut path = Vec::with_capacity(encoded.len());
    let mut i = 0;
    while i < encoded.len() {
        let hex = |at: usize| encoded.get(at).and_then(|&c| (c as char).to_digit(16));
        match (encoded[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                let byte = (high * 16 + low) as u8;
                if byte == b'/' {
                    return Err(ErrorCode::INVALID_FILE_URL_PATH.throw(global, format_args!("{MESSAGE}")));
                }
                path.push(byte);
                i += 3;
            }
            (byte, _, _) => {
                path.push(byte);
                i += 1;
            }
        }
    }
    if path.is_empty() {
        return Err(ErrorCode::INVALID_ARG_VALUE.throw(global, format_args!("{MESSAGE}")));
    }
    Ok(path)
}

/// `Bun.file(path, options)`.
pub fn file(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let [path, options] = frame.arguments_as_array::<2>();
    let Some(path) = path_or_fd_from_js(global, &path)? else {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("Expected file path string or file descriptor"),
        ));
    };
    if path.path().is_some_and(|path| path.starts_with(b"s3://")) {
        return Err(not_implemented(global, "S3 (an s3:// path)"));
    }
    let mut blob = Blob::find_or_create_file_from_path(path);
    // `BunFileOptions`: anything but an object is no options.
    if options.is_object() {
        let file_type = match options.get(global, "type")? {
            Some(text) if text.is_string() => Some(text.to_utf8(global)?).filter(|t| !t.is_empty()),
            Some(object) => match engine::string_object_value(&object) {
                Some(text) => Some(text.to_utf8(global)?),
                None => None,
            },
            None => None,
        };
        if let Some(file_type) = file_type
            && is_valid_blob_type(file_type.as_bytes())
        {
            blob.content_type_was_set = true;
            blob.content_type = BlobContentType::from_type_option(file_type.as_bytes());
        }
        if let Some(last_modified) = options.get(global, "lastModified")? {
            let n = last_modified.to_number(global)?;
            blob.last_modified = if n.is_nan() { 0.0 } else { n };
        }
    }
    blob.to_js(global)
}

pub use write_file::write;

/// What the runtime puts on `Bun`, `globalThis` and `URL`: `Blob`, `File`, `file`, `write`, `stdin`, `stdout`,
/// `stderr`, `createObjectURL`, `revokeObjectURL`, and node:buffer's `resolveObjectURL`.
pub fn exports(global: &JSGlobalObject) -> JsResult<Vec<(&'static str, JSValue)>> {
    let blob = class::constructor::<Blob>(global)?;
    let file_ctor = JSFunction::create_constructor(global, "File", file_constructor, 2);
    let prototype = blob.get(global, "prototype")?.unwrap_or_default();
    engine::define_symbol_for_method(
        global,
        &prototype,
        "nodejs.util.inspect.custom",
        inspect_custom,
    )?;
    file_ctor
        .get_object()
        .expect("a function is an object")
        .put_read_only(global, "prototype", prototype)?;
    engine::define_has_instance(global, &file_ctor, file_has_instance)?;
    engine::set_host_serializer(global, crate::web::serialize_host);
    let stdio = |fd: i32| {
        Blob::find_or_create_file_from_path(PathOrFileDescriptor::Fd(bun_sys::Fd::from_uv(fd)))
            .to_js(global)
    };
    Ok(vec![
        ("Blob", blob),
        ("File", file_ctor),
        ("FormData", form_data::constructor(global)?),
        ("file", JSFunction::create(global, "file", file, 2)),
        ("write", JSFunction::create(global, "write", write, 3)),
        ("stdin", stdio(0)?),
        ("stdout", stdio(1)?),
        ("stderr", stdio(2)?),
        (
            "createObjectURL",
            JSFunction::create(global, "createObjectURL", object_url_registry::create_object_url, 1),
        ),
        (
            "revokeObjectURL",
            JSFunction::create(global, "revokeObjectURL", object_url_registry::revoke_object_url, 1),
        ),
        (
            "resolveObjectURL",
            JSFunction::create(global, "resolveObjectURL", object_url_registry::resolve_object_url, 1),
        ),
    ])
}
