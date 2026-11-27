//! JSSQLStatement.cpp's `SQLiteBinder`: what script passes to a statement, bound to its parameters, and `fromJS`.
//!
//! The arguments are an array. Its first element decides: an array is the list of values, an object of named
//! parameters is that, and anything else makes the arguments themselves the list. A named parameter (`$a`, `:a`,
//! `@a`) is found by its name with the prefix first, then without; in strict mode the other way around, and one
//! without a value throws.

use std::cell::RefCell;
use std::num::NonZero;
use std::rc::Rc;

use bun_js::engine::property::PropertyKey;
use bun_js::{CoerceTo, JSGlobalObject, JSObject, JSValue, JsResult};
use turso_core::Value;

use crate::connection::Connection;
use crate::engine::{self, Bytes};
use crate::error::SqliteError;
use crate::statement::StatementCore;

pub struct ParameterName {
    /// "$a", ":a", "@a", "?2". `None` for a plain "?".
    prefixed: Option<String>,
    /// "a". `None` where `index` is the key.
    bare: Option<String>,
    /// The position of "?" and "?2" (from 0). 1 for "$1".
    index: u32,
}

#[derive(Default)]
pub struct BindingsMap {
    /// Empty when no parameter has a name: the key of each one is its position.
    names: Option<Rc<[ParameterName]>>,
    pub strict: bool,
}

impl BindingsMap {
    pub fn new(strict: bool) -> BindingsMap {
        BindingsMap { names: None, strict }
    }

    fn names(&mut self, statement: &turso_core::Statement) -> Rc<[ParameterName]> {
        if let Some(names) = &self.names {
            return names.clone();
        }
        let count = statement.parameters_count();
        let mut names: Vec<ParameterName> = Vec::new();
        for i in 0..count {
            let Some(name) = NonZero::new(i + 1).and_then(|index| statement.parameters().name(index))
            else {
                continue;
            };
            if names.is_empty() {
                names = (0..count)
                    .map(|j| ParameterName {
                        prefixed: None,
                        bare: None,
                        index: j as u32,
                    })
                    .collect();
            }
            let bare = name.get(1..).map(str::to_owned);
            if !name.starts_with('?') {
                match bare.as_deref().and_then(parse_index) {
                    Some(index) => names[i].index = index,
                    None => names[i].bare = bare,
                }
            }
            names[i].prefixed = Some(name);
        }
        let names: Rc<[ParameterName]> = names.into();
        self.names = Some(names.clone());
        names
    }
}

/// JavaScriptCore's `parseIndex`: a canonical array index.
fn parse_index(name: &str) -> Option<u32> {
    if name.is_empty() || (name.len() > 1 && name.starts_with('0')) {
        return None;
    }
    if !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    name.parse::<u32>().ok().filter(|&index| index != u32::MAX)
}

/// What a value is bound to: a statement of script's, or db.run()'s own.
pub enum Target<'a> {
    Statement(&'a StatementCore),
    Transient {
        stmt: &'a RefCell<turso_core::Statement>,
        connection: &'a Connection,
    },
}

impl Target<'_> {
    /// Script that ran during a property read can have finalized the statement (`finalize()`, `db.close()`).
    fn check_still_alive(&self, global: &JSGlobalObject) -> JsResult<()> {
        let alive = match self {
            Target::Statement(core) => core.is_prepared(),
            Target::Transient { connection, .. } => connection.handle().is_some(),
        };
        if alive {
            return Ok(());
        }
        let message = match self {
            Target::Statement(core) => core.finalized_message(),
            Target::Transient { .. } => "Database has closed",
        };
        Err(global.throw(format_args!("{message}")))
    }

    fn with<R>(&self, f: impl FnOnce(&mut turso_core::Statement) -> R) -> R {
        match self {
            Target::Statement(core) => {
                let mut stmt = core.stmt.borrow_mut();
                f(stmt.as_mut().expect("checked alive"))
            }
            Target::Transient { stmt, .. } => f(&mut stmt.borrow_mut()),
        }
    }
}

/// What `fromJS` makes of a value.
pub enum FromJs {
    Value(Value),
    /// Not a string, TypedArray, boolean, number, bigint or null.
    Unsupported,
}

/// `fromJS`: -0 stays REAL to keep its sign; a BigInt keeps its low 64 bits unless `safe_integers`, where one
/// outside them throws.
pub fn from_js(global: &JSGlobalObject, value: &JSValue, safe_integers: bool) -> JsResult<FromJs> {
    if let Some(number) = value.get_number() {
        let integral = number.trunc() == number && number.abs() <= 9_007_199_254_740_991.0;
        return Ok(FromJs::Value(
            if integral && !(number == 0.0 && number.is_sign_negative()) {
                Value::from_i64(number as i64)
            } else {
                Value::from_f64(number)
            },
        ));
    }
    if let Some(text) = value.as_string() {
        return Ok(FromJs::Value(Value::from_text(text)));
    }
    if value.is_undefined_or_null() {
        return Ok(FromJs::Value(Value::Null));
    }
    if value.is_boolean() {
        return Ok(FromJs::Value(Value::from_i64(i64::from(value.as_boolean()))));
    }
    if value.is_big_int() {
        if safe_integers && let Some(digits) = engine::bigint_out_of_i64(value) {
            return Err(global.throw_range_error(format_args!("BigInt value '{digits}' is out of range")));
        }
        return Ok(FromJs::Value(Value::from_i64(i64::coerce_from(value.clone(), global)?)));
    }
    match engine::bytes_of(global, value, false)? {
        Bytes::Bytes(bytes) => Ok(FromJs::Value(Value::from_blob(bytes))),
        Bytes::Detached => Ok(FromJs::Value(Value::from_blob(Vec::new()))),
        Bytes::Other => Ok(FromJs::Unsupported),
    }
}

pub struct Binder<'a> {
    pub global: &'a JSGlobalObject,
    pub target: Target<'a>,
    pub bindings: &'a RefCell<BindingsMap>,
    pub safe_integers: bool,
}

impl Binder<'_> {
    /// `arguments` is the array of what script passed. `at_prepare`: db.prepare() has always dropped values that are
    /// not a list (`texts.map(db.prepare, db)` passes the index).
    pub fn bind(&self, arguments: &JSValue, at_prepare: bool) -> JsResult<()> {
        let global = self.global;
        let outer = arguments.is_array().then(|| arguments.get_object()).flatten();
        let mut values = outer.clone();
        let mut first = arguments.clone();
        if let Some(list) = &values {
            // Nothing was passed: the values of the last call stay bound.
            if list.to_js().get_length(global)? == 0 {
                return Ok(());
            }
            first = self.element(list, 0)?;
        }

        let mut named = None;
        if values.is_some() && first.is_array() {
            values = first.get_object();
        } else if let Some(object) = first
            .get_object()
            .filter(engine::holds_named_parameters)
        {
            named = Some(object);
        } else if values.is_none() {
            return Err(global.throw_type_error(format_args!("Expected object or array")));
        }

        self.target.with(|stmt| stmt.clear_bindings());
        if let Some(named) = named {
            return self.bind_named(&named);
        }

        let values = values.expect("an array");
        let count = values.to_js().get_length(global)?;
        if count == 0 {
            return Ok(());
        }
        let required = self.target.with(|stmt| stmt.parameters_count()) as u64;
        if count != required {
            let is_outer = outer
                .as_ref()
                .is_some_and(|outer| {
                    bun_js::engine::JsObject::equals(outer.as_engine(), values.as_engine())
                });
            if at_prepare && is_outer {
                return Ok(());
            }
            return Err(global.throw(format_args!(
                "SQLite query expected {required} values, received {count}"
            )));
        }
        for i in 0..count as u32 {
            let value = self.element(&values, i)?;
            self.bind_value(i as usize + 1, &value)?;
        }
        Ok(())
    }

    fn element(&self, array: &JSObject, index: u32) -> JsResult<JSValue> {
        let value = array.get_index(self.global, index)?;
        self.target.check_still_alive(self.global)?;
        Ok(value)
    }

    /// The own property `name`, or `index` where `name` is `None`; `None` where there is none.
    fn own_property(
        &self,
        target: &JSObject,
        name: Option<&str>,
        index: u32,
    ) -> JsResult<Option<JSValue>> {
        let key = match name {
            Some(name) => engine::key(name),
            None => PropertyKey::from(index),
        };
        let value = engine::own_property(self.global, target, key)?;
        self.target.check_still_alive(self.global)?;
        Ok(value)
    }

    fn bind_named(&self, target: &JSObject) -> JsResult<()> {
        let (names, strict, count) = self.target.with(|stmt| {
            let mut bindings = self.bindings.borrow_mut();
            (bindings.names(stmt), bindings.strict, stmt.parameters_count())
        });
        for i in 0..count {
            let value = match names.get(i).filter(|name| name.prefixed.is_some()) {
                None => self.own_property(target, None, i as u32)?,
                Some(name) => {
                    let prefixed = name.prefixed.as_deref();
                    let bare = name.bare.as_deref();
                    let (first, second) = if strict {
                        ((bare, name.index), (prefixed, 0))
                    } else {
                        ((prefixed, 0), (bare, name.index))
                    };
                    match self.own_property(target, first.0, first.1)? {
                        Some(value) => Some(value),
                        None => self.own_property(target, second.0, second.1)?,
                    }
                }
            };
            let Some(value) = value else {
                if !strict {
                    continue;
                }
                return Err(self.global.throw(format_args!(
                    "Missing parameter \"{}\"",
                    missing_parameter(&names, i)
                )));
            };
            self.bind_value(i + 1, &value)?;
        }
        Ok(())
    }

    fn bind_value(&self, index: usize, value: &JSValue) -> JsResult<()> {
        let value = match from_js(self.global, value, self.safe_integers) {
            Ok(FromJs::Value(value)) => value,
            Ok(FromJs::Unsupported) => {
                return Err(self.global.throw_type_error(format_args!(
                    "Binding expected string, TypedArray, boolean, number, bigint or null"
                )));
            }
            Err(error) => {
                self.target.with(|stmt| stmt.clear_bindings());
                return Err(error);
            }
        };
        let index = NonZero::new(index).expect("from 1");
        match self.target.with(|stmt| stmt.bind_at(index, value)) {
            Ok(()) => Ok(()),
            Err(error) => Err(SqliteError::from(error).throw(self.global)),
        }
    }
}

fn missing_parameter(names: &[ParameterName], i: usize) -> String {
    let Some(name) = names.get(i).filter(|name| name.prefixed.is_some()) else {
        return format!("?{}", i + 1);
    };
    // With a numbered parameter ("?1", "$1") in the statement, names are spelled as in the SQL.
    let numbers_any = names
        .iter()
        .any(|name| name.prefixed.is_some() && name.bare.is_none());
    if numbers_any {
        name.prefixed.clone().unwrap_or_default()
    } else {
        name.bare.clone().unwrap_or_default()
    }
}
