//! `FormData` (XHR §4.3) as Bun's `DOMFormData`, and the parsers of `Blob.formData()` and `FormData.from()`:
//! src/jsc/bindings/DOMFormData.cpp, src/jsc/bindings/webcore/JSDOMFormData.cpp and src/runtime/webcore/FormData.rs
//! on `bun_js`.

use boa_gc::{Finalize, Trace};
use bun_core::form_data::Encoding;
use bun_core::strings;
use bun_http_types::MimeType as mime_type;
use bun_js::class::{self, Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, JSArray, JSGlobalObject, JSObject, JSValue, JsClass, JsResult};

use super::blob::{Blob, BlobContentType};

/// An entry's value: a string, or the `File` that `get()` answers each time.
#[derive(Clone, Trace, Finalize)]
pub enum Value {
    Text(String),
    Blob(JSValue),
}

#[derive(Clone, Trace, Finalize)]
pub struct Entry {
    pub name: String,
    pub value: Value,
}

#[derive(Default, Trace, Finalize)]
pub struct FormData {
    entries: Vec<Entry>,
}

/// A `USVString`: lone surrogates become U+FFFD.
fn usv(global: &JSGlobalObject, value: &JSValue) -> JsResult<String> {
    Ok(String::from_utf16_lossy(&value.to_utf16(global)?))
}

fn not_enough_arguments(global: &JSGlobalObject) -> bun_js::JsError {
    global.throw_type_error(format_args!("Not enough arguments"))
}

impl FormData {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn append_text(&mut self, name: String, value: String) {
        self.entries.push(Entry {
            name,
            value: Value::Text(value),
        });
    }

    pub fn append_blob(&mut self, name: String, file: JSValue) {
        self.entries.push(Entry {
            name,
            value: Value::Blob(file),
        });
    }

    /// `set`: the first entry of `name` becomes `entry`, and the others go.
    fn set(&mut self, entry: Entry) {
        match self.entries.iter().position(|e| e.name == entry.name) {
            Some(first) => {
                let name = entry.name.clone();
                self.entries[first] = entry;
                let mut index = 0;
                self.entries.retain(|e| {
                    index += 1;
                    index <= first + 1 || e.name != name
                });
            }
            None => self.entries.push(entry),
        }
    }
}

/// The entry that `append(name, value[, filename])` and `set` make, after the overload dispatch of JSDOMFormData.cpp.
fn entry_of(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Entry> {
    let count = frame.arguments_count();
    if count < 2 {
        return Err(not_enough_arguments(global));
    }
    let name = usv(global, &frame.argument(0))?;
    let value = frame.argument(1);
    let is_blob = value.as_class_ref::<Blob>().is_some();
    if count == 2 && !is_blob {
        return Ok(Entry {
            name,
            value: Value::Text(usv(global, &value)?),
        });
    }
    let Some(blob) = value.as_class_ref::<Blob>() else {
        return Err(global.throw_type_error(format_args!("Expected argument to be a Blob.")));
    };
    let filename = frame.argument(2);
    let file = file_entry(global, &value, &blob, &filename)?;
    Ok(Entry {
        name,
        value: Value::Blob(file),
    })
}

/// A File appended without a new filename is stored as it is; anything else becomes a new File of the same bytes.
fn file_entry(
    global: &JSGlobalObject,
    value: &JSValue,
    blob: &ClassRef<Blob>,
    filename: &JSValue,
) -> JsResult<JSValue> {
    let filename = match filename.is_undefined() {
        true => None,
        false => Some(usv(global, filename)?),
    };
    let mut file = blob.borrow(global)?.clone();
    if filename.is_none() && file.is_jsdom_file {
        return Ok(value.clone());
    }
    let name = filename.unwrap_or_else(|| file.name.as_ref().and_then(JSValue::as_string).unwrap_or_default());
    file.is_jsdom_file = true;
    file.name = (!name.is_empty()).then(|| JSValue::from_utf8(name.as_bytes()));
    file.to_js(global)
}

fn value_to_js(value: &Value) -> JSValue {
    match value {
        Value::Text(text) => JSValue::from_utf8(text.as_bytes()),
        Value::Blob(file) => file.clone(),
    }
}

fn name_argument(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<String> {
    if frame.arguments_count() < 1 {
        return Err(not_enough_arguments(global));
    }
    usv(global, &frame.argument(0))
}

/// What `entries()`, `keys()` and `values()` iterate: the entries as they are now.
fn iterate(
    this: &ClassRef<FormData>,
    global: &JSGlobalObject,
    each: fn(&JSGlobalObject, &Entry) -> JSValue,
) -> JsResult<JSValue> {
    let entries = this.borrow(global)?.entries.clone();
    let items: Vec<JSValue> = entries.iter().map(|entry| each(global, entry)).collect();
    let array = JSArray::create(global, &items).to_js();
    let values = array.get(global, "values")?.unwrap_or_default();
    values.call(global, array, &[])
}

fn entries(this: &ClassRef<FormData>, global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    iterate(this, global, |global, entry| {
        let name = JSValue::from_utf8(entry.name.as_bytes());
        JSArray::create(global, &[name, value_to_js(&entry.value)]).to_js()
    })
}

/// `toJSON()`, Bun's: an object of the entries, an array where a name repeats.
fn to_json(this: &ClassRef<FormData>, global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let entries = this.borrow(global)?.entries.clone();
    let mut grouped: Vec<(&str, Vec<JSValue>)> = Vec::new();
    for entry in &entries {
        let value = value_to_js(&entry.value);
        match grouped.iter_mut().find(|(name, _)| *name == entry.name) {
            Some((_, values)) => values.push(value),
            None => grouped.push((&entry.name, vec![value])),
        }
    }
    let object = JSObject::create(global);
    for (name, mut values) in grouped {
        let value = match values.len() {
            1 => values.remove(0),
            _ => JSArray::create(global, &values).to_js(),
        };
        object.put(global, name, value);
    }
    crate::engine::define_to_string_tag(global, &object.to_js(), "FormData")?;
    Ok(object.to_js())
}

impl JsClass for FormData {
    const NAME: &'static str = "FormData";
    const LENGTH: usize = 0;

    fn construct(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<FormData> {
        Ok(FormData::default())
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.method("append", 2, attributes, |this, global, frame| {
            let entry = entry_of(global, frame)?;
            this.borrow_mut(global)?.entries.push(entry);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("delete", 1, attributes, |this, global, frame| {
            let name = name_argument(global, frame)?;
            this.borrow_mut(global)?.entries.retain(|e| e.name != name);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("get", 1, attributes, |this, global, frame| {
            let name = name_argument(global, frame)?;
            let form = this.borrow(global)?;
            Ok(form
                .entries
                .iter()
                .find(|e| e.name == name)
                .map_or(JSValue::NULL, |e| value_to_js(&e.value)))
        })?;
        class.method("getAll", 1, attributes, |this, global, frame| {
            let name = name_argument(global, frame)?;
            let values: Vec<JSValue> = this
                .borrow(global)?
                .entries
                .iter()
                .filter(|e| e.name == name)
                .map(|e| value_to_js(&e.value))
                .collect();
            Ok(JSArray::create(global, &values).to_js())
        })?;
        class.method("has", 1, attributes, |this, global, frame| {
            let name = name_argument(global, frame)?;
            let has = this.borrow(global)?.entries.iter().any(|e| e.name == name);
            Ok(JSValue::js_boolean(has))
        })?;
        class.method("set", 2, attributes, |this, global, frame| {
            let entry = entry_of(global, frame)?;
            this.borrow_mut(global)?.set(entry);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("entries", 0, attributes, entries)?;
        class.method("keys", 0, attributes, |this, global, _| {
            iterate(this, global, |_, e| JSValue::from_utf8(e.name.as_bytes()))
        })?;
        class.method("values", 0, attributes, |this, global, _| {
            iterate(this, global, |_, e| value_to_js(&e.value))
        })?;
        class.method("forEach", 1, attributes, |this, global, frame| {
            let callback = frame.argument(0);
            if !callback.is_callable() {
                return Err(global.throw_type_error(format_args!(
                    "Argument 1 ('callback') to FormData.forEach must be a function"
                )));
            }
            let this_arg = frame.argument(1);
            // Live, as the WebIDL iterator is: an entry the callback appends is visited too.
            for index in 0.. {
                let entry = this.borrow(global)?.entries.get(index).cloned();
                let Some(entry) = entry else { break };
                let args = [value_to_js(&entry.value), JSValue::from_utf8(entry.name.as_bytes()), this.to_js()];
                callback.call(global, this_arg.clone(), &args)?;
            }
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("toJSON", 0, attributes, to_json)?;
        class.getter("length", Attributes { enumerable: false, ..attributes }, |this, global| {
            Ok(JSValue::js_number(this.borrow(global)?.entries.len() as f64))
        })?;
        class.static_method("from", 1, attributes, from)?;
        Ok(())
    }
}

/// The ways a multipart body is malformed, as src/runtime/error.rs words them.
#[derive(Debug, Clone, Copy)]
pub enum ParseError {
    BoundaryIsTooLong,
    MissingFinalBoundary,
    IsMissingHeaderEnd,
    IsMissingHeaderLineEnd,
    IsMissingHeaderColonSeparator,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::BoundaryIsTooLong => "boundary is too long",
            Self::MissingFinalBoundary => "missing final boundary",
            Self::IsMissingHeaderEnd => "is missing header end",
            Self::IsMissingHeaderLineEnd => "is missing header line end",
            Self::IsMissingHeaderColonSeparator => "is missing header colon separator",
        })
    }
}

/// A part of a multipart body: slices of the input.
#[derive(Default)]
pub struct Field<'a> {
    pub name: &'a [u8],
    pub value: &'a [u8],
    pub filename: &'a [u8],
    pub content_type: &'a [u8],
    pub is_file: bool,
}

/// `for_each_multipart_entry`: each part of `input` that has a name.
pub fn for_each_multipart_entry<'a>(
    input: &'a [u8],
    boundary: &[u8],
    mut iterator: impl FnMut(&Field<'a>) -> JsResult<()>,
) -> Result<JsResult<()>, ParseError> {
    if boundary.len() + 4 > 76 {
        return Err(ParseError::BoundaryIsTooLong);
    }
    let final_boundary = [b"--", boundary, b"--"].concat();
    let Some(final_index) = strings::last_index_of(input, &final_boundary) else {
        return Err(ParseError::MissingFinalBoundary);
    };
    let slice = &input[..final_index];
    let separator = [b"--", boundary, b"\r\n"].concat();
    let mut chunks: Vec<&'a [u8]> = Vec::new();
    let mut rest = slice;
    while let Some(at) = strings::index_of(rest, &separator) {
        chunks.push(&rest[..at]);
        rest = &rest[at + separator.len()..];
    }
    chunks.push(rest);
    for chunk in chunks.into_iter().skip(1) {
        let header_end = strings::index_of(chunk, b"\r\n\r\n").ok_or(ParseError::IsMissingHeaderEnd)?;
        let mut header = &chunk[..header_end + 2];
        let mut field = Field::default();
        let mut filename = None;
        while !header.is_empty() && (filename.is_none() || field.name.is_empty()) {
            let line_end = strings::index_of(header, b"\r\n").ok_or(ParseError::IsMissingHeaderLineEnd)?;
            let line = &header[..line_end];
            header = &header[line_end + 2..];
            let colon = strings::index_of(line, b":").ok_or(ParseError::IsMissingHeaderColonSeparator)?;
            let key = &line[..colon];
            let mut value: &[u8] = line.get(colon + 1..).unwrap_or_default();
            if key.eq_ignore_ascii_case(b"content-disposition") {
                value = strings::trim(value, b" \t");
                if strings::starts_with_case_insensitive_ascii(value, b"form-data;") {
                    value = strings::trim(&value[b"form-data;".len()..], b" \t");
                }
                while let Some(eq) = strings::index_of(value, b"=") {
                    let eq_key = strings::trim(&value[..eq], b" \t;");
                    value = &value[eq + 1..];
                    if value.starts_with(b"\"") {
                        value = &value[1..];
                    }
                    let mut field_value = value;
                    let mut i = 0;
                    while i < field_value.len() {
                        match field_value[i] {
                            b'"' => {
                                field_value = &field_value[..i];
                                break;
                            }
                            b'\\' => i += usize::from(field_value.get(i + 1) == Some(&b'"')),
                            _ => {}
                        }
                        i += 1;
                    }
                    value = &value[(i + 1).min(value.len())..];
                    if eq_key.eq_ignore_ascii_case(b"name") {
                        field.name = field_value;
                    } else if eq_key.eq_ignore_ascii_case(b"filename") {
                        filename = Some(field_value);
                        field.is_file = true;
                    }
                    if !field.name.is_empty() && filename.is_some() {
                        break;
                    }
                    match strings::index_of_char(value, b';') {
                        Some(semi) => value = &value[semi as usize + 1..],
                        None => break,
                    }
                }
            } else if !value.is_empty()
                && field.content_type.is_empty()
                && key.eq_ignore_ascii_case(b"content-type")
            {
                let trimmed = strings::trim(value, b"; \t");
                if super::blob::is_valid_blob_type_or_tab(trimmed) {
                    field.content_type = trimmed;
                }
            }
        }
        if field.name.is_empty() {
            continue;
        }
        let mut body = &chunk[header_end + 4..];
        if body.ends_with(b"\r\n") {
            body = &body[..body.len() - 2];
        }
        field.value = body;
        field.filename = filename.unwrap_or_default();
        if let Err(error) = iterator(&field) {
            return Ok(Err(error));
        }
    }
    Ok(Ok(()))
}

/// `application/x-www-form-urlencoded` parsing (URL §5.1): `+` is a space, `%XX` a byte, the bytes UTF-8.
pub fn parse_url_encoded(input: &[u8]) -> Vec<(String, String)> {
    let decode = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let hex = |at: usize| bytes.get(at).and_then(|&c| (c as char).to_digit(16));
            match (bytes[i], hex(i + 1), hex(i + 2)) {
                (b'+', _, _) => {
                    out.push(b' ');
                    i += 1;
                }
                (b'%', Some(high), Some(low)) => {
                    out.push((high * 16 + low) as u8);
                    i += 3;
                }
                (byte, _, _) => {
                    out.push(byte);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    let mut pairs = Vec::new();
    for sequence in strings::split(input, b"&") {
        if sequence.is_empty() {
            continue;
        }
        pairs.push(match strings::index_of_char(sequence, b'=') {
            Some(eq) => (decode(&sequence[..eq as usize]), decode(&sequence[eq as usize + 1..])),
            None => (decode(sequence), String::new()),
        });
    }
    pairs
}

/// `FormData::to_js`: a new `FormData` of what `input` holds in `encoding`.
pub fn parse(global: &JSGlobalObject, input: &[u8], encoding: &Encoding) -> JsResult<JSValue> {
    let mut form = FormData::default();
    match encoding {
        Encoding::URLEncoded => {
            for (name, value) in parse_url_encoded(strings::without_utf8_bom(input)) {
                form.append_text(name, value);
            }
        }
        Encoding::Multipart(boundary) => {
            let parsed = for_each_multipart_entry(input, boundary, |field| {
                let name = String::from_utf8_lossy(field.name).into_owned();
                if !field.is_file {
                    let value = String::from_utf8_lossy(strings::without_utf8_bom(field.value)).into_owned();
                    form.append_text(name, value);
                    return Ok(());
                }
                let mut blob = Blob::init(field.value.to_vec());
                if !field.content_type.is_empty() {
                    blob.content_type = BlobContentType::from_lowercased(field.content_type);
                    blob.content_type_was_set = true;
                } else {
                    let extension = bun_paths::extension(field.filename);
                    let mime = extension
                        .get(1..)
                        .filter(|ext| !ext.is_empty())
                        .and_then(mime_type::by_extension_no_default)
                        .or_else(|| mime_type::sniff(field.value));
                    if let Some(mime) = mime {
                        blob.content_type = BlobContentType::from_mime(&mime);
                        blob.content_type_was_set = false;
                    }
                }
                blob.is_jsdom_file = true;
                blob.name = (!field.filename.is_empty()).then(|| JSValue::from_utf8(field.filename));
                form.append_blob(name, blob.to_js(global)?);
                Ok(())
            });
            match parsed {
                Ok(result) => result?,
                Err(error) => {
                    return Err(global.throw(format_args!("FormData encoding failed: {error}")));
                }
            }
        }
    }
    class::to_js(global, form)
}

/// `FormData.from(input, boundary)`: a string, a buffer or a Blob; without a boundary, or with an empty one, the
/// input is URL-encoded. A Blob that is a file is empty.
fn from(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let [input, boundary] = frame.arguments_as_array::<2>();
    let bytes = match input.as_class_ref::<Blob>() {
        Some(blob) => blob.borrow(global)?.shared_view().to_vec(),
        None => bytes_of(global, &input, "input")?,
    };
    let encoding = match boundary.is_undefined_or_null() {
        true => Encoding::URLEncoded,
        false => match bytes_of(global, &boundary, "boundary")? {
            boundary if boundary.is_empty() => Encoding::URLEncoded,
            boundary => Encoding::Multipart(boundary.into_boxed_slice()),
        },
    };
    parse(global, &bytes, &encoding)
}

/// A string as UTF-8, or the bytes of a buffer.
fn bytes_of(global: &JSGlobalObject, value: &JSValue, name: &str) -> JsResult<Vec<u8>> {
    if let Some(bytes) = crate::engine::buffer_source_bytes(global, value) {
        return Ok(bytes);
    }
    if value.is_string() {
        return Ok(value.to_utf8(global)?.into_bytes());
    }
    Err(bun_js::ErrorCode::INVALID_ARG_TYPE.throw(
        global,
        format_args!("{name} must be a string, ArrayBuffer or ArrayBufferView"),
    ))
}

/// `Blob.formData()` of the bytes the Blob holds: its type says how they are encoded.
pub fn of_blob_bytes(global: &JSGlobalObject, content_type: &[u8], bytes: &[u8]) -> JsResult<JSValue> {
    let Some(encoding) = Encoding::get(content_type) else {
        return Err(global.throw(format_args!("Invalid encoding")));
    };
    parse(global, bytes, &encoding)
}

/// `FormData`, for `globalThis`.
pub fn constructor(global: &JSGlobalObject) -> JsResult<JSValue> {
    let constructor = class::constructor::<FormData>(global)?;
    let prototype = constructor.get(global, "prototype")?.unwrap_or_default();
    let entries = prototype.get(global, "entries")?.unwrap_or_default();
    crate::engine::define_iterator(global, &prototype, entries)?;
    crate::engine::define_to_string_tag(global, &prototype, "FormData")?;
    Ok(constructor)
}
