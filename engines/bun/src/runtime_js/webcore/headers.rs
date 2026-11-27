//! `Headers` (Fetch §5.2) as Bun's `FetchHeaders`: names are compared without case, `get` combines values with
//! ", ", iteration is sorted by lowercase name with each `set-cookie` on its own, and Bun's `toJSON()`, `count`
//! and `getAll("set-cookie")`.

use boa_engine::object::builtins::JsArray;
use boa_engine::{Context, JsResult, JsString, JsSymbol, JsValue};
use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsClass};

use crate::{cx, lift};

/// The fields of a request or response, in the order they were added, with the case they were given in.
#[derive(Clone, Debug, Default)]
pub struct HeaderList {
    entries: Vec<(String, String)>,
}

fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
}

/// HTTP whitespace (Fetch §2.2) at the ends is not part of a value.
fn normalize(value: &str) -> &str {
    value.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
}

/// WebCore's `isValidHTTPHeaderValue`: a value is bytes, so nothing above U+00FF.
fn is_valid_value(value: &str) -> bool {
    !value.chars().any(|c| matches!(c, '\0' | '\r' | '\n') || u32::from(c) > 0xFF)
}

/// The names WebCore knows (`HTTPHeaderName`), as `HTTPHeaderNames.in` spells them.
fn known_names() -> impl Iterator<Item = &'static str> {
    include_str!("../../jsc/bindings/webcore/HTTPHeaderNames.in")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
}

/// The spelling of a name WebCore knows, which its messages use.
pub fn known_name(name: &str) -> Option<&'static str> {
    known_names().find(|known| known.eq_ignore_ascii_case(name))
}

impl HeaderList {
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> HeaderList {
        HeaderList {
            entries: pairs.into_iter().collect(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<String> {
        let mut values = self
            .entries
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .peekable();
        values.peek()?;
        let separator = if name.eq_ignore_ascii_case("cookie") { "; " } else { ", " };
        Some(values.collect::<Vec<_>>().join(separator))
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }

    pub fn append(&mut self, name: &str, value: &str) {
        self.entries.push((name.to_owned(), value.to_owned()));
    }

    pub fn set(&mut self, name: &str, value: &str) {
        match self.entries.iter().position(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some(first) => {
                self.entries[first].1 = value.to_owned();
                let mut index = 0;
                self.entries.retain(|(n, _)| {
                    let keep = index <= first || !n.eq_ignore_ascii_case(name);
                    index += 1;
                    keep
                });
            }
            None => self.append(name, value),
        }
    }

    pub fn delete(&mut self, name: &str) {
        self.entries.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub fn set_cookies(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// The lowercase names other than `set-cookie`, each once, in the order they were first added.
    fn distinct_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for (name, _) in &self.entries {
            let name = name.to_ascii_lowercase();
            if name != "set-cookie" && !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    /// `HTTPHeaderMap::size()`: a field per name, and one per `set-cookie`.
    pub fn count(&self) -> usize {
        self.distinct_names().len() + self.set_cookies().len()
    }

    /// Bun's "sort and combine": lowercase names in order with their values combined, then each `set-cookie` (Fetch
    /// sorts it in with the others).
    pub fn sorted(&self) -> Vec<(String, String)> {
        let mut names = self.distinct_names();
        names.sort();
        let mut out: Vec<(String, String)> = names
            .into_iter()
            .filter_map(|name| self.get(&name).map(|value| (name, value)))
            .collect();
        out.extend(self.set_cookies().into_iter().map(|value| ("set-cookie".to_owned(), value)));
        out
    }

    /// `toJSON()`'s order, WebCore's map's: the names it knows, then `set-cookie`, then the others, each in the
    /// order it was first added.
    pub fn json_order(&self) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let (known, other): (Vec<String>, Vec<String>) =
            self.distinct_names().into_iter().partition(|name| known_name(name).is_some());
        let with_values = |names: Vec<String>| {
            names
                .into_iter()
                .filter_map(|name| self.get(&name).map(|value| (name, value)))
                .collect::<Vec<_>>()
        };
        (with_values(known), with_values(other))
    }
}

#[derive(Trace, Finalize, Default)]
pub struct Headers {
    #[unsafe_ignore_trace]
    pub list: HeaderList,
}

fn invalid_name(global: &JSGlobalObject, name: &str) -> bun_js::JsError {
    global.throw_type_error(format_args!("Invalid header name: '{name}'"))
}

/// A field from script: a valid name, and a value without HTTP whitespace at its ends.
pub fn checked(global: &JSGlobalObject, name: &JSValue, value: &JSValue) -> bun_js::JsResult<(String, String)> {
    let name = name.to_utf8(global)?;
    if !is_token(&name) {
        return Err(invalid_name(global, &name));
    }
    let value = value.to_utf8(global)?;
    let value = normalize(&value).to_owned();
    check_value(global, &name, &value)?;
    Ok((name, value))
}

/// WebCore's `canWriteHeader`: a known name is spelled as it knows it in the message.
pub fn check_value(global: &JSGlobalObject, name: &str, value: &str) -> bun_js::JsResult<()> {
    if is_valid_value(value) {
        return Ok(());
    }
    let name = known_name(name).unwrap_or(name);
    Err(global.throw_type_error(format_args!("Header '{name}' has invalid value")))
}

fn iterate(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<Vec<JSValue>> {
    let context = cx(global);
    // `Array.from`, the built-in one: it runs the iterator protocol.
    let result = (|| -> JsResult<Vec<JSValue>> {
        let array = context.intrinsics().constructors().array().constructor();
        let from = array.get(JsString::from("from"), context)?;
        let items = from
            .as_callable()
            .ok_or_else(|| boa_engine::JsNativeError::typ().with_message("Array.from"))?
            .call(&array.clone().into(), &[value.as_engine().clone()], context)?;
        let items = items
            .as_object()
            .ok_or_else(|| boa_engine::JsNativeError::typ().with_message("Array.from"))?;
        let length = JsArray::from_object(items.clone())?.length(context)?;
        (0..length)
            .map(|i| items.get(i, context).map(JSValue::from_engine))
            .collect()
    })();
    result.map_err(|error| bun_js_throw(global, error))
}

pub(crate) fn bun_js_throw(global: &JSGlobalObject, error: boa_engine::JsError) -> bun_js::JsError {
    let context = cx(global);
    match error.into_opaque(context) {
        Ok(value) => global.throw_value(JSValue::from_engine(value)),
        Err(error) => global.throw(format_args!("{error}")),
    }
}

/// `HeadersInit`: a `Headers`, an iterable of pairs, or a record.
pub fn list_from_init(global: &JSGlobalObject, init: &JSValue) -> bun_js::JsResult<HeaderList> {
    let mut list = HeaderList::default();
    if init.is_undefined_or_null() {
        return Ok(list);
    }
    if let Some(headers) = init.as_class_ref::<Headers>() {
        return Ok(headers.borrow(global)?.list.clone());
    }
    let Some(object) = init.get_object() else {
        return Err(global.throw_type_error(format_args!(
            "Failed to construct 'Headers': The provided value is not of type 'HeadersInit'"
        )));
    };
    let has_iterator = {
        let context = cx(global);
        let result = object.as_engine().get(JsSymbol::iterator(), context);
        lift_bun(global, result)?.is_callable()
    };
    if has_iterator {
        for pair in iterate(global, init)? {
            if pair.get_object().is_none() {
                return Err(global.throw_type_error(format_args!(
                    "Failed to construct 'Headers': The provided value cannot be converted to a sequence."
                )));
            }
            let items = iterate(global, &pair)?;
            if items.len() != 2 {
                return Err(global.throw_type_error(format_args!(
                    "Failed to construct 'Headers': Header sub-sequence must contain exactly two items"
                )));
            }
            let (name, value) = checked(global, &items[0], &items[1])?;
            list.append(&name, &value);
        }
        return Ok(list);
    }
    for key in object.keys(global)? {
        let value = object.get(global, key.as_bytes())?;
        let (name, value) = checked(global, &JSValue::from_utf8(key.as_bytes()), &value)?;
        list.append(&name, &value);
    }
    Ok(list)
}

pub(crate) fn lift_bun<T>(global: &JSGlobalObject, result: JsResult<T>) -> bun_js::JsResult<T> {
    result.map_err(|error| bun_js_throw(global, error))
}

fn name_argument(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<String> {
    let name = frame.argument(0).to_utf8(global)?;
    if !is_token(&name) {
        return Err(invalid_name(global, &name));
    }
    Ok(name)
}

fn string_array(global: &JSGlobalObject, values: impl IntoIterator<Item = JsValue>) -> JSValue {
    JSValue::from_engine(JsArray::from_iter(values, cx(global)).into())
}

fn str_value(s: &str) -> JsValue {
    JsString::from(s).into()
}

const METHOD: Attributes = Attributes {
    writable: true,
    enumerable: true,
    configurable: true,
};

impl JsClass for Headers {
    const NAME: &'static str = "Headers";

    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<Self> {
        Ok(Headers {
            list: list_from_init(global, &frame.argument(0))?,
        })
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> bun_js::JsResult<()> {
        class.method("append", 2, METHOD, |this, global, frame| {
            let (name, value) = checked(global, &frame.argument(0), &frame.argument(1))?;
            this.borrow_mut(global)?.list.append(&name, &value);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("set", 2, METHOD, |this, global, frame| {
            let (name, value) = checked(global, &frame.argument(0), &frame.argument(1))?;
            this.borrow_mut(global)?.list.set(&name, &value);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("delete", 1, METHOD, |this, global, frame| {
            let name = name_argument(global, frame)?;
            this.borrow_mut(global)?.list.delete(&name);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("get", 1, METHOD, |this, global, frame| {
            let name = name_argument(global, frame)?;
            Ok(match this.borrow(global)?.list.get(&name) {
                Some(value) => JSValue::from_utf8(value.as_bytes()),
                None => JSValue::NULL,
            })
        })?;
        class.method("has", 1, METHOD, |this, global, frame| {
            let name = name_argument(global, frame)?;
            Ok(JSValue::js_boolean(this.borrow(global)?.list.has(&name)))
        })?;
        class.method("getSetCookie", 0, METHOD, |this, global, _| {
            let cookies = this.borrow(global)?.list.set_cookies();
            Ok(string_array(global, cookies.iter().map(|c| str_value(c))))
        })?;
        class.method("getAll", 1, METHOD, |this, global, frame| {
            let name = name_argument(global, frame)?;
            if !name.eq_ignore_ascii_case("set-cookie") {
                return Err(global.throw_type_error(format_args!(
                    "Only \"set-cookie\" is supported."
                )));
            }
            let cookies = this.borrow(global)?.list.set_cookies();
            Ok(string_array(global, cookies.iter().map(|c| str_value(c))))
        })?;
        // Each step reads the list as it is then, at the same index: what the callback changes is seen.
        class.method("forEach", 1, METHOD, |this, global, frame| {
            let callback = frame.argument(0);
            if !callback.is_callable() {
                return Err(ErrorCode::INVALID_ARG_TYPE
                    .throw(global, format_args!("Cannot call callback on a non-function")));
            }
            let this_arg = frame.argument(1);
            let mut index = 0;
            loop {
                let entry = this.borrow(global)?.list.sorted().into_iter().nth(index);
                let Some((name, value)) = entry else { break };
                index += 1;
                callback.call(
                    global,
                    this_arg.clone(),
                    &[JSValue::from_utf8(value.as_bytes()), JSValue::from_utf8(name.as_bytes()), this.to_js()],
                )?;
            }
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("entries", 0, METHOD, |this, global, _| iterator(this, global, Kind::Entries))?;
        class.method("keys", 0, METHOD, |this, global, _| iterator(this, global, Kind::Keys))?;
        class.method("values", 0, METHOD, |this, global, _| iterator(this, global, Kind::Values))?;
        class.method("toJSON", 0, METHOD, |this, global, _| {
            let list = this.borrow(global)?.list.clone();
            let object = bun_js::JSObject::create(global);
            let (known, other) = list.json_order();
            for (name, value) in known {
                object.put(global, name.as_bytes(), JSValue::from_utf8(value.as_bytes()));
            }
            let cookies = list.set_cookies();
            if !cookies.is_empty() {
                object.put(global, "set-cookie", string_array(global, cookies.iter().map(|c| str_value(c))));
            }
            for (name, value) in other {
                object.put(global, name.as_bytes(), JSValue::from_utf8(value.as_bytes()));
            }
            Ok(object.to_js())
        })?;
        class.getter("count", Attributes::DEFAULT, |this, global| {
            Ok(JSValue::js_number(this.borrow(global)?.list.count() as f64))
        })?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Keys,
    Values,
    Entries,
}

/// FetchHeaders' iterator: an index into the sorted list as it is at each step, so that a change made while it
/// walks is seen (`FetchHeaders::Iterator::nextEntry`).
#[derive(Trace, Finalize)]
pub struct HeadersIterator {
    /// The `Headers`, which the iterator keeps alive.
    headers: JSValue,
    #[unsafe_ignore_trace]
    kind: Kind,
    #[unsafe_ignore_trace]
    index: usize,
    #[unsafe_ignore_trace]
    done: bool,
}

fn iterator(this: &bun_js::ClassRef<Headers>, global: &JSGlobalObject, kind: Kind) -> bun_js::JsResult<JSValue> {
    bun_js::class::to_js(
        global,
        HeadersIterator {
            headers: this.to_js(),
            kind,
            index: 0,
            done: false,
        },
    )
}

impl JsClass for HeadersIterator {
    const NAME: &'static str = "Headers Iterator";

    fn define(class: &mut ClassBuilder<'_, Self>) -> bun_js::JsResult<()> {
        let hidden = Attributes {
            writable: true,
            enumerable: false,
            configurable: true,
        };
        class.method("next", 0, hidden, |this, global, _| {
            let (headers, kind, index, done) = {
                let state = this.borrow(global)?;
                (state.headers.clone(), state.kind, state.index, state.done)
            };
            let entry = match headers.as_class_ref::<Headers>() {
                Some(headers) if !done => headers.borrow(global)?.list.sorted().into_iter().nth(index),
                _ => None,
            };
            let mut state = this.borrow_mut(global)?;
            let value = match entry {
                Some((name, value)) => {
                    state.index += 1;
                    match kind {
                        Kind::Keys => str_value(&name),
                        Kind::Values => str_value(&value),
                        Kind::Entries => JsArray::from_iter([str_value(&name), str_value(&value)], cx(global)).into(),
                    }
                }
                None => {
                    state.done = true;
                    JsValue::undefined()
                }
            };
            let done = state.done;
            drop(state);
            let result = boa_engine::builtins::iterable::create_iter_result_object(value, done, cx(global));
            Ok(JSValue::from_engine(result))
        })
    }
}

/// A new `Headers` with `list`.
pub fn create(global: &JSGlobalObject, list: HeaderList) -> bun_js::JsResult<JSValue> {
    bun_js::class::to_js(global, Headers { list })
}

pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let global = JSGlobalObject::from_context(context);
    let constructor = lift(global, bun_js::class::constructor::<Headers>(global))?;
    let prototype = constructor.as_engine().as_object().map(|c| c.get(JsString::from("prototype"), context));
    if let Some(prototype) = prototype.transpose()?.and_then(|p| p.as_object()) {
        let entries = prototype.get(JsString::from("entries"), context)?;
        prototype.define_property_or_throw(
            JsSymbol::iterator(),
            boa_engine::property::PropertyDescriptor::builder()
                .value(entries)
                .writable(true)
                .enumerable(false)
                .configurable(true),
            context,
        )?;
    }
    let global = JSGlobalObject::from_context(context);
    let iterator = lift(global, bun_js::class::constructor::<HeadersIterator>(global))?;
    let iterator = iterator.as_engine().as_object().map(|c| c.get(JsString::from("prototype"), context));
    if let Some(prototype) = iterator.transpose()?.and_then(|p| p.as_object()) {
        // %IteratorPrototype%, which gives `[Symbol.iterator]()` and the helpers.
        let iterator_prototype = context.intrinsics().objects().iterator_prototypes().array().prototype();
        prototype.set_prototype(iterator_prototype);
    }
    crate::define_global(context, "Headers", constructor.into_engine())
}
