//! `Bun.deepEquals`: C++'s `Bun__deepEquals` (src/jsc/bindings/bindings.cpp) in the modes `Bun.deepEquals` runs it
//! in, without asymmetric matchers and without Node's prototype checks (those are `bun:test`'s and `node:util`'s).
//!
//! As in C++, an object compared with itself through a cycle is equal where the other side has the same cycle, a
//! getter runs where the comparison reads a property, and nothing else runs script.

use boa_engine::builtins::error::Error as ErrorData;
use boa_engine::builtins::typed_array::TypedArrayKind;
use boa_engine::object::builtins::{
    JsArray, JsArrayBuffer, JsDate, JsMap, JsRegExp, JsSet, JsSharedArrayBuffer, JsTypedArray,
};
use boa_engine::property::PropertyKey;
use boa_engine::{JsBigInt, JsObject, JsString, JsSymbol, JsValue, js_string};

use crate::{JSGlobalObject, JSValue, JsResult};

/// How `Bun.deepEquals(a, b, strict, skipPrototype)` compares.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeepEqualsMode {
    /// `toStrictEqual`'s rules: `undefined` properties count, sparse arrays differ from dense ones, classes must match.
    pub strict: bool,
    /// With `strict`: the class (and a `String` subclass) is not compared.
    pub skip_prototype_identity: bool,
}

impl JSValue {
    /// `Bun.deepEquals(self, other)`.
    pub fn deep_equals(&self, other: &JSValue, global: &JSGlobalObject) -> JsResult<bool> {
        self.deep_equals_with(other, global, DeepEqualsMode::default())
    }

    /// `Bun.deepEquals(self, other, true)`.
    pub fn strict_deep_equals(&self, other: &JSValue, global: &JSGlobalObject) -> JsResult<bool> {
        let mode = DeepEqualsMode {
            strict: true,
            skip_prototype_identity: false,
        };
        self.deep_equals_with(other, global, mode)
    }

    pub fn deep_equals_with(
        &self,
        other: &JSValue,
        global: &JSGlobalObject,
        mode: DeepEqualsMode,
    ) -> JsResult<bool> {
        let mut comparison = Comparison {
            global,
            mode,
            stack: Vec::new(),
        };
        comparison.equals(Some(&self.0), Some(&other.0), true)
    }
}

/// How deep the values can be nested: JavaScriptCore throws a stack overflow where its stack ends.
const MAX_DEPTH: usize = 2_000;

struct Comparison<'g> {
    global: &'g JSGlobalObject,
    mode: DeepEqualsMode,
    /// The pairs of objects being compared, outermost first.
    stack: Vec<(JsObject, JsObject)>,
}

/// Which object of the two enumerated a name that the other does not list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnumeratedBy {
    First,
    Second,
}

impl Comparison<'_> {
    fn lift<T>(&self, result: boa_engine::JsResult<T>) -> JsResult<T> {
        self.global.lift(result)
    }

    fn get(&self, object: &JsObject, key: &PropertyKey) -> JsResult<JsValue> {
        let result = object.get(key.clone(), self.global.cx());
        self.lift(result)
    }

    /// `None` stands for C++'s empty value: a hole, or an element that is an accessor.
    fn equals(&mut self, v1: Option<&JsValue>, v2: Option<&JsValue>, add_to_stack: bool) -> JsResult<bool> {
        let (v1, v2) = match (v1, v2) {
            (Some(v1), Some(v2)) => (v1, v2),
            (v1, v2) => return Ok(v1.is_none() == v2.is_none()),
        };
        if JsValue::same_value(v1, v2) {
            return Ok(true);
        }
        let (Some(o1), Some(o2)) = (v1.as_object(), v2.as_object()) else {
            return Ok(false);
        };
        for (first, second) in &self.stack {
            let first_matches = JsObject::equals(first, &o1);
            let second_matches = JsObject::equals(second, &o2);
            if first_matches {
                return Ok(second_matches);
            }
            if second_matches {
                return Ok(false);
            }
        }
        if self.stack.len() >= MAX_DEPTH {
            return Err(self
                .global
                .throw_range_error(format_args!("Maximum call stack size exceeded.")));
        }
        let depth = self.stack.len();
        if add_to_stack {
            self.stack.push((o1.clone(), o2.clone()));
        }
        let result = self.objects_equal(&o1, &o2);
        self.stack.truncate(depth);
        result
    }

    fn objects_equal(&mut self, o1: &JsObject, o2: &JsObject) -> JsResult<bool> {
        if let Some(equal) = self.special_objects_equal(o1, o2)? {
            return Ok(equal);
        }
        if let Some(equal) = self.special_objects_equal(o2, o1)? {
            return Ok(equal);
        }
        if o1.is_array() != o2.is_array() {
            return Ok(false);
        }
        if o1.is_array() {
            return self.arrays_equal(o1, o2);
        }
        if self.mode.strict
            && !self.mode.skip_prototype_identity
            && class_name(o1) != class_name(o2)
        {
            return Ok(false);
        }
        let (names1, names2) = if is_plain(o1) && is_plain(o2) {
            (self.own_enumerable_keys(o1, |_| true)?, self.own_enumerable_keys(o2, |_| true)?)
        } else {
            (self.property_names(o1)?, self.property_names(o2)?)
        };
        self.enumerated_properties_equal(o1, &names1, o2, &names2)
    }

    fn arrays_equal(&mut self, o1: &JsObject, o2: &JsObject) -> JsResult<bool> {
        let length1 = self.array_length(o1)?;
        let length2 = self.array_length(o2)?;
        if self.mode.strict && length1 != length2 {
            return Ok(false);
        }
        let mut i = 0;
        while i < length1 {
            let left = index_without_accessors(o1, i);
            let right = index_without_accessors(o2, i);
            if self.mode.strict {
                match (&left, &right) {
                    (None, None) => {
                        i += 1;
                        continue;
                    }
                    (None, _) | (_, None) => return Ok(false),
                    _ => {}
                }
            } else if (left.is_none() || right.is_none())
                && (left.as_ref().is_some_and(JsValue::is_undefined)
                    || right.as_ref().is_some_and(JsValue::is_undefined))
            {
                i += 1;
                continue;
            }
            if !self.equals(left.as_ref(), right.as_ref(), true)? {
                return Ok(false);
            }
            i += 1;
        }
        while i < length2 {
            match index_without_accessors(o2, i) {
                None => {}
                Some(right) if right.is_undefined() => {}
                Some(_) => return Ok(false),
            }
            i += 1;
        }
        let names1 = self.own_enumerable_keys(o1, |key| matches!(key, PropertyKey::Symbol(_)))?;
        let names2 = self.own_enumerable_keys(o2, |key| matches!(key, PropertyKey::Symbol(_)))?;
        if names1.is_empty() && names2.is_empty() {
            return Ok(true);
        }
        self.enumerated_properties_equal(o1, &names1, o2, &names2)
    }

    fn array_length(&self, array: &JsObject) -> JsResult<u64> {
        let array = JsArray::from_object(array.clone()).expect("is_array() said so");
        let result = array.length(self.global.cx());
        self.lift(result)
    }

    /// The own enumerable keys `filter` keeps, in `[[OwnPropertyKeys]]` order.
    fn own_enumerable_keys(
        &self,
        object: &JsObject,
        filter: impl Fn(&PropertyKey) -> bool,
    ) -> JsResult<Vec<PropertyKey>> {
        let keys = object.own_property_keys(self.global.cx());
        let mut names = Vec::new();
        for key in self.lift(keys)? {
            if !filter(&key) {
                continue;
            }
            let descriptor = object.get_own_property(key.clone(), self.global.cx());
            if self.lift(descriptor)?.is_some_and(|descriptor| descriptor.expect_enumerable()) {
                names.push(key);
            }
        }
        Ok(names)
    }

    /// C++'s `getPropertyNames`: the enumerable keys of the object and of its prototypes, each name once.
    fn property_names(&self, object: &JsObject) -> JsResult<Vec<PropertyKey>> {
        let mut names: Vec<PropertyKey> = Vec::new();
        let mut holder = Some(object.clone());
        while let Some(current) = holder {
            for key in self.own_enumerable_keys(&current, |_| true)? {
                if !names.contains(&key) {
                    names.push(key);
                }
            }
            holder = current.prototype();
        }
        Ok(names)
    }

    fn enumerated_properties_equal(
        &mut self,
        o1: &JsObject,
        names1: &[PropertyKey],
        o2: &JsObject,
        names2: &[PropertyKey],
    ) -> JsResult<bool> {
        if self.mode.strict && names1.len() != names2.len() {
            return Ok(false);
        }
        let mut paired = 0;
        for name in names1 {
            let equal = if names2.contains(name) {
                paired += 1;
                self.paired_property_equal(o1, o2, name)?
            } else {
                self.unpaired_property_equal(o1, o2, name, EnumeratedBy::First)?
            };
            if !equal {
                return Ok(false);
            }
        }
        if paired == names2.len() {
            return Ok(true);
        }
        for name in names2 {
            if names1.contains(name) {
                continue;
            }
            if !self.unpaired_property_equal(o1, o2, name, EnumeratedBy::Second)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn paired_property_equal(&mut self, o1: &JsObject, o2: &JsObject, name: &PropertyKey) -> JsResult<bool> {
        if is_stack_of_error(o1, name) {
            return Ok(true);
        }
        let value1 = self.get(o1, name)?;
        let value2 = self.get(o2, name)?;
        self.equals(Some(&value1), Some(&value2), true)
    }

    fn unpaired_property_equal(
        &mut self,
        o1: &JsObject,
        o2: &JsObject,
        name: &PropertyKey,
        enumerated_by: EnumeratedBy,
    ) -> JsResult<bool> {
        if self.mode.strict {
            return Ok(false);
        }
        if is_stack_of_error(o1, name) {
            return Ok(true);
        }
        let in_first = enumerated_by == EnumeratedBy::First;
        let (own, other) = if in_first { (o1, o2) } else { (o2, o1) };
        let value = self.get(own, name)?;
        if value.is_undefined() {
            return Ok(true);
        }
        // How `message`, `cause` or `line` was set decides if an Error enumerates it.
        if !o1.is::<ErrorData>() || *name == PropertyKey::from(js_string!("__proto__")) {
            return Ok(false);
        }
        let has = other.has_property(name.clone(), self.global.cx());
        if !self.lift(has)? {
            return Ok(false);
        }
        let other_value = self.get(other, name)?;
        if in_first {
            self.equals(Some(&value), Some(&other_value), true)
        } else {
            self.equals(Some(&other_value), Some(&value), true)
        }
    }

    /// C++'s `specialObjectsDequal`: an answer for the objects whose contents are not their properties, or `None` to
    /// compare their properties.
    fn special_objects_equal(&mut self, c1: &JsObject, c2: &JsObject) -> JsResult<Option<bool>> {
        if let Ok(set1) = JsSet::from_object(c1.clone()) {
            let Ok(set2) = JsSet::from_object(c2.clone()) else {
                return Ok(Some(false));
            };
            if set1.size() != set2.size() {
                return Ok(Some(false));
            }
            let keys1 = self.set_values(&set1)?;
            let keys2 = self.set_values(&set2)?;
            for key1 in &keys1 {
                if set2.has(key1.clone()) {
                    continue;
                }
                let mut found = false;
                for key2 in &keys2 {
                    if self.equals(Some(key1), Some(key2), false)? {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Ok(Some(false));
                }
            }
            return Ok(Some(true));
        }
        if let Ok(map1) = JsMap::from_object(c1.clone()) {
            let Ok(map2) = JsMap::from_object(c2.clone()) else {
                return Ok(Some(false));
            };
            let entries1 = self.map_entries(&map1)?;
            let entries2 = self.map_entries(&map2)?;
            if entries1.len() != entries2.len() {
                return Ok(Some(false));
            }
            for (key1, value1) in &entries1 {
                let found = map2.get(key1.clone(), self.global.cx());
                let mut value2 = self.lift(found)?;
                if value2.is_undefined() {
                    let mut found = false;
                    for (key2, value) in &entries2 {
                        if self.equals(Some(key1), Some(key2), false)? {
                            value2 = value.clone();
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        return Ok(Some(false));
                    }
                }
                if !self.equals(Some(value1), Some(&value2), false)? {
                    return Ok(Some(false));
                }
            }
            return Ok(Some(true));
        }
        if is_array_buffer(c1) {
            if !is_array_buffer(c2) {
                return Ok(Some(false));
            }
            if is_shared_array_buffer(c1) != is_shared_array_buffer(c2) {
                return Ok(Some(false));
            }
            let left = crate::array_buffer::buffer_bytes(self.global, Some(c1))?;
            let right = crate::array_buffer::buffer_bytes(self.global, Some(c2))?;
            let (Some(left), Some(right)) = (left, right) else {
                // Node wraps each in `new Uint8Array(buffer)`, which throws for a detached one.
                return Err(self.global.throw_type_error(format_args!(
                    "Cannot perform Construct on a detached ArrayBuffer"
                )));
            };
            return Ok(Some(left == right));
        }
        if let Ok(date1) = JsDate::from_object(c1.clone()) {
            let Ok(date2) = JsDate::from_object(c2.clone()) else {
                return Ok(Some(false));
            };
            let time1 = date1.get_time(self.global.cx());
            let time1 = self.lift(time1)?;
            let time2 = date2.get_time(self.global.cx());
            let time2 = self.lift(time2)?;
            return Ok(Some(time1.as_number() == time2.as_number()));
        }
        if let Ok(regexp1) = JsRegExp::from_object(c1.clone()) {
            let Ok(regexp2) = JsRegExp::from_object(c2.clone()) else {
                return Ok(Some(false));
            };
            let source1 = regexp1.source(self.global.cx());
            let source2 = regexp2.source(self.global.cx());
            let flags1 = regexp1.flags(self.global.cx());
            let flags2 = regexp2.flags(self.global.cx());
            let same = self.lift(source1)? == self.lift(source2)? && self.lift(flags1)? == self.lift(flags2)?;
            return Ok(Some(same));
        }
        if let Some(kind1) = c1.downcast_ref::<ErrorData>().map(|error| error.kind()) {
            let Some(kind2) = c2.downcast_ref::<ErrorData>().map(|error| error.kind()) else {
                return Ok(Some(false));
            };
            return self.errors_equal(c1, c2, kind1 == kind2).map(Some);
        }
        if let Ok(left) = JsTypedArray::from_object(c1.clone()) {
            return self.typed_arrays_equal(c1, &left, c2);
        }
        if let Some(string1) = c1.downcast_ref::<JsString>().map(|string| (*string).clone()) {
            let Some(string2) = c2.downcast_ref::<JsString>().map(|string| (*string).clone()) else {
                return Ok(Some(false));
            };
            if !self.mode.skip_prototype_identity && class_name(c1) != class_name(c2) {
                return Ok(Some(false));
            }
            if string1 != string2 {
                return Ok(Some(false));
            }
            if self.mode.strict && (self.has_extra_own_properties(c1)? || self.has_extra_own_properties(c2)?) {
                return Ok(None);
            }
            return Ok(Some(true));
        }
        if c1.is_callable() {
            return Ok(Some(false));
        }
        // Number, Boolean, Symbol and BigInt wrappers: the same kind of wrapper of the same value, then their
        // properties.
        macro_rules! wrapper {
            ($t:ty) => {
                if let Some(value1) = c1.downcast_ref::<$t>().map(|value| JsValue::from((*value).clone())) {
                    let Some(value2) = c2.downcast_ref::<$t>().map(|value| JsValue::from((*value).clone())) else {
                        return Ok(Some(false));
                    };
                    if !JsValue::same_value(&value1, &value2) {
                        return Ok(Some(false));
                    }
                    return Ok(None);
                }
            };
        }
        wrapper!(f64);
        wrapper!(bool);
        wrapper!(JsSymbol);
        wrapper!(JsBigInt);
        Ok(None)
    }

    fn errors_equal(&mut self, left: &JsObject, right: &JsObject, same_kind: bool) -> JsResult<bool> {
        if !same_kind {
            return Ok(false);
        }
        let name_key = PropertyKey::from(js_string!("name"));
        let message_key = PropertyKey::from(js_string!("message"));
        let cause_key = PropertyKey::from(js_string!("cause"));
        if self.sanitized_string(left, &name_key)? != self.sanitized_string(right, &name_key)? {
            return Ok(false);
        }
        if self.sanitized_string(left, &message_key)? != self.sanitized_string(right, &message_key)? {
            return Ok(false);
        }
        if self.mode.strict {
            let left_has = left.has_property(cause_key.clone(), self.global.cx());
            let left_has = self.lift(left_has)?;
            let right_has = right.has_property(cause_key.clone(), self.global.cx());
            if left_has != self.lift(right_has)? {
                return Ok(false);
            }
        }
        let left_cause = self.get(left, &cause_key)?;
        let right_cause = self.get(right, &cause_key)?;
        if !self.equals(Some(&left_cause), Some(&right_cause), true)? {
            return Ok(false);
        }
        let names1 = self.property_names(left)?;
        let names2 = self.property_names(right)?;
        self.enumerated_properties_equal(left, &names1, right, &names2)
    }

    /// What an error prints as its `name` or `message`: the property as a string.
    fn sanitized_string(&self, error: &JsObject, key: &PropertyKey) -> JsResult<JsString> {
        let value = self.get(error, key)?;
        if let Some(string) = value.as_string() {
            return Ok(string.clone());
        }
        if value.is_undefined() {
            return Ok(js_string!());
        }
        let result = value.to_string(self.global.cx());
        self.lift(result)
    }

    fn typed_arrays_equal(
        &mut self,
        c1: &JsObject,
        left: &JsTypedArray,
        c2: &JsObject,
    ) -> JsResult<Option<bool>> {
        let Ok(right) = JsTypedArray::from_object(c2.clone()) else {
            return Ok(Some(false));
        };
        let kind = left.kind();
        if kind.is_none() || kind != right.kind() {
            return Ok(Some(false));
        }
        let compare_own_properties =
            self.mode.strict && (self.has_extra_own_properties(c1)? || self.has_extra_own_properties(c2)?);
        let view1 = JSValue(c1.clone().into()).array_buffer_view_bytes(self.global)?;
        let view2 = JSValue(c2.clone().into()).array_buffer_view_bytes(self.global)?;
        let (Some(bytes1), Some(bytes2)) = (view1, view2) else {
            return Ok(Some(false));
        };
        if bytes1.len() != bytes2.len() {
            return Ok(Some(false));
        }
        // Loose, floats compare as numbers (+0 is -0, NaN is not NaN); anything else by its bytes.
        let is_integer = matches!(
            kind,
            Some(
                TypedArrayKind::Int8
                    | TypedArrayKind::Uint8
                    | TypedArrayKind::Uint8Clamped
                    | TypedArrayKind::Int16
                    | TypedArrayKind::Uint16
                    | TypedArrayKind::Int32
                    | TypedArrayKind::Uint32
                    | TypedArrayKind::BigInt64
                    | TypedArrayKind::BigUint64
            )
        );
        let loose_float = !self.mode.strict && !is_integer;
        let contents_equal = if loose_float {
            let length = left.length(self.global.cx());
            let length = self.lift(length)?;
            let mut equal = true;
            for i in 0..length {
                let a = left.at(i as i64, self.global.cx());
                let a = self.lift(a)?;
                let b = right.at(i as i64, self.global.cx());
                let b = self.lift(b)?;
                if a.as_number() != b.as_number() {
                    equal = false;
                    break;
                }
            }
            equal
        } else {
            bytes1 == bytes2
        };
        if !contents_equal {
            return Ok(Some(false));
        }
        if compare_own_properties {
            return Ok(None);
        }
        Ok(Some(true))
    }

    /// C++'s `hasExtraOwnProperties` of a typed array or a `String` object: an own property its contents do not
    /// make.
    fn has_extra_own_properties(&self, object: &JsObject) -> JsResult<bool> {
        let length = match object.downcast_ref::<JsString>() {
            Some(string) => Some(string.len()),
            None => None,
        };
        let keys = object.own_property_keys(self.global.cx());
        Ok(self.lift(keys)?.iter().any(|key| match key {
            PropertyKey::Index(index) => length.is_some_and(|length| index.get() as usize >= length),
            PropertyKey::String(name) => !(length.is_some() && *name == js_string!("length")),
            PropertyKey::Symbol(_) => true,
        }))
    }

    fn set_values(&self, set: &JsSet) -> JsResult<Vec<JsValue>> {
        let mut values = Vec::new();
        let result = set.for_each_native(|value| {
            values.push(value);
            Ok(())
        });
        self.lift(result)?;
        Ok(values)
    }

    fn map_entries(&self, map: &JsMap) -> JsResult<Vec<(JsValue, JsValue)>> {
        let mut entries = Vec::new();
        let result = map.for_each_native(|key, value| {
            entries.push((key, value));
            Ok(())
        });
        self.lift(result)?;
        Ok(entries)
    }
}

fn is_shared_array_buffer(object: &JsObject) -> bool {
    JsSharedArrayBuffer::from_object(object.clone()).is_ok()
}

fn is_array_buffer(object: &JsObject) -> bool {
    JsArrayBuffer::from_object(object.clone()).is_ok() || is_shared_array_buffer(object)
}

/// An ordinary object without elements, whose own properties are what C++'s fast path compares.
fn is_plain(object: &JsObject) -> bool {
    object.is::<boa_engine::builtins::object::OrdinaryObject>()
        && object.borrow().properties().index_property_keys().next().is_none()
}

fn is_stack_of_error(object: &JsObject, name: &PropertyKey) -> bool {
    object.is::<ErrorData>() && *name == PropertyKey::from(js_string!("stack"))
}

/// C++'s `getIndexWithoutAccessors`: an own element that is data, `None` for a hole or an accessor.
fn index_without_accessors(object: &JsObject, index: u64) -> Option<JsValue> {
    let key = PropertyKey::from(index);
    object
        .borrow()
        .properties()
        .get(&key)
        .and_then(|descriptor| descriptor.value().cloned())
}

/// `JSObject::calculatedClassName`: the name of the constructor its prototype has, else what kind of object it is.
fn class_name(object: &JsObject) -> JsString {
    let own_data = |holder: &JsObject, key: &str| {
        holder
            .borrow()
            .properties()
            .get(&PropertyKey::from(JsString::from(key)))
            .and_then(|descriptor| descriptor.value().cloned())
    };
    let name = object
        .prototype()
        .and_then(|prototype| own_data(&prototype, "constructor"))
        .and_then(|constructor| constructor.as_object())
        .and_then(|constructor| own_data(&constructor, "name"))
        .and_then(|name| name.as_string().filter(|name| !name.is_empty()));
    match name {
        Some(name) => name,
        None if object.is_array() => js_string!("Array"),
        None if object.is_callable() => js_string!("Function"),
        None => js_string!("Object"),
    }
}
