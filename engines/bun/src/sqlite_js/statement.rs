//! `SQLStatement`: a prepared statement, as JSSQLStatement.cpp's `JSSQLStatement` and its prototype.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::engine::property::PropertyKey;
use bun_js::{CallFrame, ClassRef, JSGlobalObject, JSValue, JsClass, JsResult};
use turso_core::{LimboError, Numeric, StepResult, Value};

use crate::binder::{Binder, BindingsMap, Target};
use crate::connection::Connection;
use crate::engine;
use crate::error::SqliteError;
use crate::function::Stepping;

/// What a statement is apart from its JavaScript object: the database's connection finalizes it on `close()`.
pub struct StatementCore {
    pub stmt: RefCell<Option<turso_core::Statement>>,
    pub connection: Rc<Connection>,
    /// Made by db.query(): `close(false)` finalizes these, but not those of db.prepare().
    pub owned_by_database: Cell<bool>,
    finalized_by_close: Cell<bool>,
    pub use_big_int64: Cell<bool>,
    /// A number only this run of the statement has.
    runs: Cell<u32>,
    pub bindings: RefCell<BindingsMap>,
    /// The column names of the last row shape, and the key of each column in a row object.
    shape: RefCell<Option<(Vec<String>, Vec<PropertyKey>)>>,
}

impl StatementCore {
    pub fn new(stmt: turso_core::Statement, connection: Rc<Connection>) -> Rc<StatementCore> {
        let core = Rc::new(StatementCore {
            stmt: RefCell::new(Some(stmt)),
            connection: connection.clone(),
            owned_by_database: Cell::new(false),
            finalized_by_close: Cell::new(false),
            use_big_int64: Cell::new(false),
            runs: Cell::new(0),
            bindings: RefCell::new(BindingsMap::default()),
            shape: RefCell::new(None),
        });
        connection.add_statement(&core);
        core
    }

    /// A statement that is stepping (a user function runs) is prepared.
    pub fn is_prepared(&self) -> bool {
        self.stmt.try_borrow().map_or(true, |stmt| stmt.is_some())
    }

    pub fn finalized_message(&self) -> &'static str {
        if self.finalized_by_close.get() {
            "Database has closed"
        } else {
            "Statement has finalized"
        }
    }

    pub fn finalize_by_close(&self) {
        if self.stmt.borrow_mut().take().is_some() {
            self.finalized_by_close.set(true);
        }
    }

    pub fn finalize(&self) {
        let stmt = self.stmt.borrow_mut().take();
        drop(stmt);
        self.connection.close_if_drained();
    }

    fn with<R>(&self, f: impl FnOnce(&mut turso_core::Statement) -> R) -> R {
        f(self
            .stmt
            .borrow_mut()
            .as_mut()
            .expect("CHECK_PREPARED ran"))
    }
}

impl Drop for StatementCore {
    fn drop(&mut self) {
        let stmt = self.stmt.get_mut().take();
        if crate::connection::thread_is_exiting() {
            let _ = std::mem::ManuallyDrop::new(stmt);
            return;
        }
        drop(stmt);
        self.connection.close_if_drained();
    }
}

/// `sqlite3_step`: a row (`true`) or the end; the engine's I/O runs to completion meanwhile. User functions run in
/// `global`.
pub fn step(global: &JSGlobalObject, stmt: &mut turso_core::Statement) -> Result<bool, LimboError> {
    let _stepping = Stepping::enter(global);
    loop {
        match stmt.step_with_structured_errors()? {
            StepResult::Row => return Ok(true),
            StepResult::Done => return Ok(false),
            StepResult::IO | StepResult::Yield | StepResult::Sleep { .. } => stmt._io().step()?,
            StepResult::Interrupt => return Err(LimboError::Interrupt),
            StepResult::Busy => return Err(LimboError::Busy),
        }
    }
}

/// `sqlite3_reset`. A statement that writes and stopped after a row (`INSERT … RETURNING`) runs to its end first:
/// SQLite has made all its changes by its first row, and the reset commits them.
pub fn reset(global: &JSGlobalObject, stmt: &mut turso_core::Statement) -> Result<(), LimboError> {
    let mut finished = Ok(());
    if stmt.is_busy() && !stmt.get_program().is_readonly() {
        finished = loop {
            match step(global, stmt) {
                Ok(true) => continue,
                Ok(false) => break Ok(()),
                Err(error) => break Err(error),
            }
        };
    }
    let reset = stmt.reset();
    finished.and(reset)
}

/// The rows that a statement which can write changed itself, as `changesSince`: `sqlite3_changes64()` keeps the
/// count of an earlier statement when this one counted nothing.
pub fn changes_since(conn: &turso_core::Connection, total_changes_before: i64) -> i64 {
    if conn.total_changes() == total_changes_before {
        0
    } else {
        conn.changes()
    }
}

/// `toJS` of a column (or a function's argument).
pub fn to_js(global: &JSGlobalObject, value: &Value, use_big_int64: bool) -> JsResult<JSValue> {
    Ok(match value {
        Value::Null => JSValue::NULL,
        Value::Numeric(Numeric::Integer(integer)) if use_big_int64 => engine::bigint(*integer),
        Value::Numeric(Numeric::Integer(integer)) => JSValue::js_number(*integer as f64),
        Value::Numeric(Numeric::Float(float)) => JSValue::js_number(f64::from(*float)),
        Value::Text(text) => JSValue::from_utf8(text.as_str().as_bytes()),
        Value::Blob(blob) => engine::uint8_array(global, blob)?,
    })
}

/// raw(): each column as the bytes SQLite has for it.
fn to_js_as_buffer(global: &JSGlobalObject, value: &Value) -> JsResult<JSValue> {
    match value {
        Value::Null => Ok(JSValue::NULL),
        Value::Numeric(Numeric::Integer(integer)) => engine::uint8_array(global, &integer.to_le_bytes()),
        Value::Numeric(Numeric::Float(float)) => {
            engine::uint8_array(global, &f64::from(*float).to_ne_bytes())
        }
        Value::Text(text) => engine::uint8_array(global, text.as_str().as_bytes()),
        Value::Blob(blob) => engine::uint8_array(global, blob),
    }
}

/// Sets `in_callback` while script runs from inside a statement of the connection.
pub struct CallbackScope<'a> {
    connection: &'a Connection,
    was: bool,
}

impl<'a> CallbackScope<'a> {
    pub fn new(connection: &'a Connection) -> CallbackScope<'a> {
        let was = connection.in_callback.replace(true);
        CallbackScope { connection, was }
    }
}

impl Drop for CallbackScope<'_> {
    fn drop(&mut self) {
        self.connection.in_callback.set(self.was);
    }
}

pub fn throw_busy(global: &JSGlobalObject) -> bun_js::JsError {
    global.throw_type_error(format_args!(
        "This database connection is busy executing a query"
    ))
}

#[derive(Trace, Finalize)]
pub struct SQLStatement {
    #[unsafe_ignore_trace]
    pub core: Rc<StatementCore>,
    /// The `SQLDatabase` handle, kept alive while the statement is.
    _database: JSValue,
    user_prototype: Option<JSValue>,
    /// Bun.sql's `transform.column.from`: makes the key of a row object from the name of a column.
    column_name_transform: Option<JSValue>,
}

impl SQLStatement {
    pub fn new(core: Rc<StatementCore>, database: JSValue) -> SQLStatement {
        SQLStatement {
            core,
            _database: database,
            user_prototype: None,
            column_name_transform: None,
        }
    }
}

type This = ClassRef<SQLStatement>;

fn core_of(this: &This, global: &JSGlobalObject) -> JsResult<Rc<StatementCore>> {
    Ok(this.borrow(global)?.core.clone())
}

/// `CHECK_PREPARED`.
fn prepared(this: &This, global: &JSGlobalObject) -> JsResult<Rc<StatementCore>> {
    let core = core_of(this, global)?;
    if !core.is_prepared() {
        return Err(global.throw(format_args!("{}", core.finalized_message())));
    }
    if core.connection.in_callback.get() {
        return Err(throw_busy(global));
    }
    Ok(core)
}

/// Throws what the engine failed with, after the statement's run ends (`StatementRun`).
fn fail(global: &JSGlobalObject, core: &StatementCore, error: LimboError) -> bun_js::JsError {
    fail_quietly(core);
    SqliteError::from(error).throw(global)
}

/// The run that starts here takes the cursor from the one before it.
fn reset_and_bind(this: &This, global: &JSGlobalObject, arguments: &JSValue) -> JsResult<Rc<StatementCore>> {
    let core = prepared(this, global)?;
    if let Err(error) = core.with(|stmt| reset(global, stmt)) {
        return Err(SqliteError::from(error).throw(global));
    }
    core.runs.set(core.runs.get().wrapping_add(1));
    if !arguments.is_undefined() {
        Binder {
            global,
            target: Target::Statement(&core),
            bindings: &core.bindings,
            safe_integers: core.use_big_int64.get(),
        }
        .bind(arguments, false)?;
    }
    Ok(core)
}

/// Steps once; an error ends the run.
fn step_once(global: &JSGlobalObject, core: &StatementCore) -> JsResult<bool> {
    match core.with(|stmt| step(global, stmt)) {
        Ok(row) => Ok(row),
        Err(error) => Err(fail(global, core, error)),
    }
}

/// After a step that returned a row: the first one of a run, or one after `as()`.
fn update_row_shape(this: &This, global: &JSGlobalObject, core: &StatementCore) -> JsResult<()> {
    let names: Vec<String> = core.with(|stmt| {
        (0..stmt.num_columns())
            .map(|i| stmt.get_column_name(i).into_owned())
            .collect()
    });
    if core
        .shape
        .borrow()
        .as_ref()
        .is_some_and(|(shaped, _)| *shaped == names)
    {
        return Ok(());
    }
    let transform = this.borrow(global)?.column_name_transform.clone();
    let keys = match transform {
        None => names.iter().map(|name| engine::key(name)).collect(),
        Some(transform) => {
            // The statement is on a row.
            let _scope = CallbackScope::new(&core.connection);
            let mut keys = Vec::with_capacity(names.len());
            for name in &names {
                let key = transform.call(global, JSValue::UNDEFINED, &[JSValue::from_utf8(name.as_bytes())]);
                let key = match key.and_then(|key| key.to_utf8(global)) {
                    Ok(key) => key,
                    Err(error) => {
                        *core.shape.borrow_mut() = None;
                        return Err(error);
                    }
                };
                keys.push(engine::key(&key));
            }
            keys
        }
    };
    *core.shape.borrow_mut() = Some((names, keys));
    Ok(())
}

/// The row the statement is on, as an object.
fn construct_result_object(this: &This, global: &JSGlobalObject, core: &StatementCore) -> JsResult<JSValue> {
    let prototype = this.borrow(global)?.user_prototype.clone();
    let object = engine::object_with_prototype(global, prototype.as_ref());
    let big = core.use_big_int64.get();
    let shape = core.shape.borrow();
    let keys = shape.as_ref().map_or(&[][..], |(_, keys)| keys.as_slice());
    let stmt = core.stmt.borrow();
    let row = stmt.as_ref().and_then(|stmt| stmt.row());
    if let Some(row) = row {
        for (i, key) in keys.iter().enumerate().take(row.len()) {
            let value = to_js(global, row.get_value(i), big)?;
            engine::put(global, &object, key, value);
        }
    }
    Ok(object.to_js())
}

/// The next row, or null at the end.
fn step_iterator(this: &This, global: &JSGlobalObject, core: &StatementCore, first: bool) -> JsResult<JSValue> {
    if !step_once(global, core)? {
        return Ok(JSValue::NULL);
    }
    // Without names: as() in the loop body.
    if first || core.shape.borrow().is_none() {
        if let Err(error) = update_row_shape(this, global, core) {
            fail_quietly(core);
            return Err(error);
        }
    }
    construct_result_object(this, global, core).inspect_err(|_| fail_quietly(core))
}

/// Ends the run, as `StatementRun`'s destructor, for an exception that is pending already.
fn fail_quietly(core: &StatementCore) {
    if let Some(stmt) = core.stmt.borrow_mut().as_mut() {
        let _ = stmt.reset();
    }
}

/// values() and raw(): a row is the array of what `to_value` makes of each column.
fn collect_arrays(
    global: &JSGlobalObject,
    core: &StatementCore,
    to_value: impl Fn(&JSGlobalObject, &Value) -> JsResult<JSValue>,
) -> JsResult<JSValue> {
    let rows = bun_js::JSArray::create(global, &[]);
    while step_once(global, core)? {
        let row = {
            let stmt = core.stmt.borrow();
            let row = stmt.as_ref().and_then(|stmt| stmt.row());
            let mut values = Vec::new();
            if let Some(row) = row {
                values.reserve(row.len());
                for i in 0..row.len() {
                    values.push(to_value(global, row.get_value(i))?);
                }
            }
            JSValue::create_array_from_slice(global, &values)
        };
        let row = row.inspect_err(|_| fail_quietly(core))?;
        rows.push(global, row).inspect_err(|_| fail_quietly(core))?;
    }
    Ok(rows.to_js())
}

fn run(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = reset_and_bind(this, global, &frame.argument(1))?;
    let Some(conn) = core.connection.engine() else {
        return Err(global.throw(format_args!("Database has closed")));
    };
    // COMMIT and RELEASE are read-only, but they flush what changed, which moves the total.
    let can_write = !core.with(|stmt| stmt.get_program().is_readonly());
    let before = if can_write { conn.total_changes() } else { 0 };
    while step_once(global, &core)? {}
    let changes = if can_write { changes_since(&conn, before) } else { 0 };
    crate::sql::put_changes(global, &frame.argument(0), &conn, changes, core.use_big_int64.get())?;
    Ok(JSValue::UNDEFINED)
}

fn get(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = reset_and_bind(this, global, &frame.argument(0))?;
    let row = step_iterator(this, global, &core, true)?;
    if row.is_null() {
        return Ok(row);
    }
    // The other rows are not read. For INSERT ... RETURNING this is the commit.
    if let Err(error) = core.with(|stmt| reset(global, stmt)) {
        return Err(SqliteError::from(error).throw(global));
    }
    Ok(row)
}

fn all(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = reset_and_bind(this, global, &frame.argument(0))?;
    let rows = bun_js::JSArray::create(global, &[]);
    if step_once(global, &core)? {
        if let Err(error) = update_row_shape(this, global, &core) {
            fail_quietly(&core);
            return Err(error);
        }
        loop {
            let row = construct_result_object(this, global, &core).inspect_err(|_| fail_quietly(&core))?;
            rows.push(global, row).inspect_err(|_| fail_quietly(&core))?;
            if !step_once(global, &core)? {
                break;
            }
        }
    }
    Ok(rows.to_js())
}

fn iterate(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = reset_and_bind(this, global, &frame.argument(0))?;
    // iterateNext() goes on from this row.
    step_iterator(this, global, &core, true)
}

fn iterate_next(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    let same_run = frame.argument(0).get_number() == Some(f64::from(core.runs.get()));
    // Not busy: a call that failed to bind has reset the statement without a new run.
    if !same_run || !core.with(|stmt| stmt.is_busy()) {
        return Err(global.throw(format_args!(
            "This iterator was invalidated: its statement was run again. Use db.prepare() to get a statement with a cursor of its own."
        )));
    }
    step_iterator(this, global, &core, false)
}

fn iterate_end(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = core_of(this, global)?;
    if !core.is_prepared() {
        return Ok(JSValue::UNDEFINED);
    }
    if core.connection.in_callback.get() {
        return Err(throw_busy(global));
    }
    // For INSERT ... RETURNING that was left after a row, the reset is the commit, and it can fail.
    if frame.argument(0).get_number() == Some(f64::from(core.runs.get()))
        && let Err(error) = core.with(|stmt| reset(global, stmt))
    {
        return Err(SqliteError::from(error).throw(global));
    }
    Ok(JSValue::UNDEFINED)
}

fn values(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = reset_and_bind(this, global, &frame.argument(0))?;
    let big = core.use_big_int64.get();
    collect_arrays(global, &core, |global, value| to_js(global, value, big))
}

fn raw(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = reset_and_bind(this, global, &frame.argument(0))?;
    collect_arrays(global, &core, to_js_as_buffer)
}

fn as_class(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = core_of(this, global)?;
    if core.connection.in_callback.get() {
        return Err(throw_busy(global));
    }
    let class = frame.argument(0);
    let prototype = if class.is_undefined()
        || class.is_strict_equal(engine::object_constructor(global), global)?
    {
        None
    } else if class.is_object() {
        if !engine::is_constructor(&class) {
            return Err(global.throw_type_error(format_args!("Expected a constructor")));
        }
        let Some(prototype) = class.get(global, "prototype")? else {
            return Err(global.throw_type_error(format_args!(
                "Expected constructor to have a prototype"
            )));
        };
        if !prototype.is_object() {
            return Err(global.throw_type_error(format_args!(
                "Expected a constructor prototype to be an object"
            )));
        }
        Some(prototype)
    } else {
        return Err(global.throw_type_error(format_args!(
            "Expected class to be a constructor or undefined"
        )));
    };
    this.borrow_mut(global)?.user_prototype = prototype;
    *core.shape.borrow_mut() = None;
    Ok(JSValue::UNDEFINED)
}

fn finalize(this: &This, global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = core_of(this, global)?;
    if core.is_prepared() {
        if core.connection.in_callback.get() {
            return Err(throw_busy(global));
        }
        core.finalize();
    }
    Ok(JSValue::UNDEFINED)
}

fn to_string(this: &This, global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = core_of(this, global)?;
    let Ok(stmt) = core.stmt.try_borrow() else {
        return Err(throw_busy(global));
    };
    Ok(match stmt.as_ref() {
        Some(stmt) => JSValue::from_utf8(stmt.expanded_sql().as_bytes()),
        None => JSValue::js_empty_string(global),
    })
}

fn columns(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    let names: Vec<JSValue> = core.with(|stmt| {
        (0..stmt.num_columns())
            .map(|i| JSValue::from_utf8(stmt.get_column_name(i).as_bytes()))
            .collect()
    });
    JSValue::create_array_from_slice(global, &names)
}

fn columns_count(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    Ok(JSValue::js_number(core.with(|stmt| stmt.num_columns()) as f64))
}

fn params_count(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    Ok(JSValue::js_number(core.with(|stmt| stmt.parameters_count()) as f64))
}

fn is_finalized(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    Ok(JSValue::js_boolean(!core_of(this, global)?.is_prepared()))
}

fn readonly(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    Ok(JSValue::js_boolean(core.with(|stmt| stmt.get_program().is_readonly())))
}

fn run_count(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    Ok(JSValue::js_number(f64::from(core.runs.get())))
}

fn column_types(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    // The types are those of the first row, so the statement runs. Only one that writes nothing may.
    if !core.with(|stmt| stmt.get_program().is_readonly()) {
        return Err(global.throw(format_args!(
            "columnTypes is not available for non-read-only statements (INSERT, UPDATE, DELETE, etc.)"
        )));
    }
    if let Err(error) = core.with(|stmt| reset(global, stmt)) {
        return Err(SqliteError::from(error).throw(global));
    }
    core.runs.set(core.runs.get().wrapping_add(1));
    let has_row = step_once(global, &core)?;
    // The count comes after the step, which can prepare the statement again with other columns.
    let types: Vec<JSValue> = core.with(|stmt| {
        let count = stmt.num_columns();
        let row = if has_row { stmt.row() } else { None };
        (0..count)
            .map(|i| {
                // Without a row, every column is NULL.
                let name = match row.filter(|row| i < row.len()).map(|row| row.get_value(i)) {
                    Some(Value::Numeric(Numeric::Integer(_))) => "INTEGER",
                    Some(Value::Numeric(Numeric::Float(_))) => "FLOAT",
                    Some(Value::Text(_)) => "TEXT",
                    Some(Value::Blob(_)) => "BLOB",
                    Some(Value::Null) | None => "NULL",
                };
                JSValue::from_utf8(name.as_bytes())
            })
            .collect()
    });
    fail_quietly(&core);
    JSValue::create_array_from_slice(global, &types)
}

fn declared_types(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    let types: Vec<JSValue> = core.with(|stmt| {
        (0..stmt.num_columns())
            .map(|i| match stmt.get_column_decltype(i) {
                Some(declared) => JSValue::from_utf8(declared.as_bytes()),
                None => JSValue::NULL,
            })
            .collect()
    });
    JSValue::create_array_from_slice(global, &types)
}

fn get_safe_integers(this: &This, global: &JSGlobalObject) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    Ok(JSValue::js_boolean(core.use_big_int64.get()))
}

fn set_safe_integers(this: &This, global: &JSGlobalObject, value: JSValue) -> JsResult<()> {
    let core = prepared(this, global)?;
    core.use_big_int64.set(value.to_boolean());
    Ok(())
}

fn set_column_name_transform(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    let value = frame.argument(0);
    if !value.is_undefined_or_null() && !value.is_callable() {
        return Err(global.throw_type_error(format_args!("Expected a function or null")));
    }
    this.borrow_mut(global)?.column_name_transform = value.is_callable().then_some(value);
    *core.shape.borrow_mut() = None;
    Ok(JSValue::UNDEFINED)
}

/// For Bun.sql: runs what follows the statement in `sql`, the text that it was prepared from. A later statement with
/// a parameter is refused. How many rows changed, or undefined when nothing follows.
fn run_tail(this: &This, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let core = prepared(this, global)?;
    let Some(sql) = frame.argument(0).as_string() else {
        return Err(global.throw(format_args!("Expected SQL string")));
    };
    let own_length = core.with(|stmt| stmt.tail_offset()).min(sql.len());
    let changes = crate::sql::run_statements(global, &core.connection, &sql, own_length, &JSValue::UNDEFINED, 0, true)?;
    Ok(changes.map_or(JSValue::UNDEFINED, |changes| JSValue::js_number(changes as f64)))
}

impl JsClass for SQLStatement {
    const NAME: &'static str = "SQLStatement";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let function = Attributes {
            writable: true,
            enumerable: true,
            configurable: true,
        };
        let private = Attributes {
            writable: false,
            enumerable: false,
            configurable: false,
        };
        for (name, length, method) in [
            ("run", 1, run as bun_js::class::Method<Self>),
            ("get", 1, get),
            ("all", 1, all),
            ("iterate", 1, iterate),
            ("iterateNext", 1, iterate_next),
            ("iterateEnd", 1, iterate_end),
            ("as", 1, as_class),
            ("values", 1, values),
            ("raw", 1, raw),
            ("finalize", 0, finalize),
            ("toString", 0, to_string),
        ] {
            class.method(name, length, function, method)?;
        }
        // Only builtin modules can name these (`$setColumnNameTransform`): statement.native is public.
        class.method("__intrinsic__setColumnNameTransform", 1, private, set_column_name_transform)?;
        class.method("__intrinsic__runTail", 1, private, run_tail)?;
        for (name, getter) in [
            ("runCount", run_count as bun_js::class::Getter<Self>),
            ("columns", columns),
            ("columnsCount", columns_count),
            ("paramsCount", params_count),
            ("isFinalized", is_finalized),
            ("readonly", readonly),
            ("columnTypes", column_types),
            ("declaredTypes", declared_types),
        ] {
            class.getter(name, function, getter)?;
        }
        class.accessor("safeIntegers", function, get_safe_integers, set_safe_integers)?;
        crate::sql::define_statics(class)
    }
}

/// A new statement's object.
pub fn create(global: &JSGlobalObject, core: Rc<StatementCore>, database: JSValue) -> JsResult<JSValue> {
    bun_js::class::to_js(global, SQLStatement::new(core, database))
}
