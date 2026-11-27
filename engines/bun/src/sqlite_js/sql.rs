//! The constructor's functions (`SQL.open`, `SQL.prepare`, `SQL.run`, …) and `SQLDatabase`, the handle in a
//! Database's `#handle`, as JSSQLStatement.cpp's `JSSQLStatementConstructor` and `JSSQLDatabase`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsClass, JsResult};
use turso_core::LimboError;

use crate::binder::{Binder, BindingsMap, Target};
use crate::connection::Connection;
use crate::engine::{self, Bytes};
use crate::error::{SqliteError, errstr};
use crate::function;
use crate::statement::{self, SQLStatement, StatementCore, changes_since, step, throw_busy};

const SAFE_INTEGERS_FLAG: i32 = 1 << 1;
const STRICT_FLAG: i32 = 1 << 2;
const OWNED_BY_DATABASE_FLAG: i32 = 1 << 3;

const SQLITE_OPEN_READWRITE: i32 = 0x2;
const DEFAULT_SQLITE_FLAGS: i32 = SQLITE_OPEN_READWRITE | crate::connection::SQLITE_OPEN_CREATE;
const MAX_SQLITE_PREPARE_FLAG: i32 = 0x1 | 0x2 | 0x4;
/// What Turso answers for every opcode of `sqlite3_file_control`: its VFS knows none (`SQLITE_NOTFOUND`).
const SQLITE_NOTFOUND: i32 = 12;

const SQLITE_DETERMINISTIC: i32 = 0x800;

const SQLITE_INSERT: i32 = 18;
const SQLITE_UPDATE: i32 = 23;
const SQLITE_DELETE: i32 = 9;

#[derive(Trace, Finalize)]
pub struct SQLDatabase {
    #[unsafe_ignore_trace]
    pub connection: Rc<Connection>,
    /// db.onInsert, db.onUpdate, db.onDelete.
    update_hooks: [Option<JSValue>; 3],
}

impl JsClass for SQLDatabase {
    const NAME: &'static str = "SQLDatabase";

    fn define(_: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        Ok(())
    }
}

fn database(global: &JSGlobalObject, connection: Rc<Connection>) -> JsResult<JSValue> {
    bun_js::class::to_js(
        global,
        SQLDatabase {
            connection,
            update_hooks: [None, None, None],
        },
    )
}

/// `databaseForHandle`: the handle is an argument of the call, which keeps it alive.
fn connection_for_handle(global: &JSGlobalObject, handle: &JSValue) -> JsResult<Rc<Connection>> {
    let Some(database) = handle.as_class_ref::<SQLDatabase>() else {
        return Err(global.throw(format_args!("Invalid database handle")));
    };
    let connection = database.borrow(global)?.connection.clone();
    if connection.in_callback.get() {
        return Err(throw_busy(global));
    }
    Ok(connection)
}

/// `toCString`: for a string that SQLite reads up to its first NUL, which must be the end.
fn to_c_string(global: &JSGlobalObject, argument: &str, value: &JSValue) -> JsResult<String> {
    let string = value.to_utf8(global)?;
    if string.contains('\0') {
        let received = string.replace('\\', "\\\\").replace('\'', "\\'").replace('\0', "\\x00");
        return Err(ErrorCode::INVALID_ARG_VALUE.throw(
            global,
            format_args!(
                "The argument '{argument}' must be a string without null bytes. Received '{received}'"
            ),
        ));
    }
    Ok(string)
}

/// SQLite reads SQL text up to its first NUL.
fn up_to_nul(sql: &str) -> &str {
    match bun_core::strings::index_of_char_usize(sql.as_bytes(), 0) {
        Some(end) => &sql[..end],
        None => sql,
    }
}

fn int32_flags(value: &JSValue) -> i32 {
    if value.is_int32() { value.as_int32() } else { 0 }
}

/// `putChanges`: `changes` and `lastInsertRowid` in the tuple's internal fields.
pub fn put_changes(
    global: &JSGlobalObject,
    tuple: &JSValue,
    conn: &turso_core::Connection,
    changes: i64,
    safe_integers: bool,
) -> JsResult<()> {
    let Some(tuple) = tuple.get_object() else {
        return Ok(());
    };
    let rowid = conn.last_insert_rowid();
    let rowid = if safe_integers {
        engine::bigint(rowid)
    } else {
        JSValue::js_number(rowid as f64)
    };
    tuple.put_internal_field(global, 0, JSValue::js_number(changes as f64))?;
    tuple.put_internal_field(global, 1, rowid)
}

fn is_skipped_in_query(c: u8) -> bool {
    c == b' ' || c == b';' || (b'\t'..=b'\r').contains(&c)
}

fn has_no_statement(error: &LimboError) -> bool {
    matches!(error, LimboError::InvalidArgument(message) if message == "The supplied SQL string contains no statements")
}

/// Runs the statements of `sql` from `head` on; the first of them gets `bindings`. How many rows they changed, or
/// `None` when there was no statement. db.run() has always run a statement that nothing was bound to with NULL;
/// `refuse_parameters` throws instead.
pub fn run_statements(
    global: &JSGlobalObject,
    connection: &Connection,
    sql: &str,
    mut head: usize,
    bindings: &JSValue,
    internal_flags: i32,
    refuse_parameters: bool,
) -> JsResult<Option<i64>> {
    let mut changes: Option<i64> = None;
    let bytes = sql.as_bytes();
    loop {
        while head < bytes.len() && is_skipped_in_query(bytes[head]) {
            head += 1;
        }
        if head >= bytes.len() {
            break;
        }
        let Some(conn) = connection.engine() else {
            return Err(global.throw(format_args!("Database has closed")));
        };
        let stmt = match conn.prepare(&sql[head..]) {
            Ok(stmt) => stmt,
            Err(error) if has_no_statement(&error) => break,
            Err(error) => return Err(SqliteError::from(error).at(head).throw(global)),
        };
        if refuse_parameters && stmt.parameters_count() > 0 {
            return Err(global.throw(format_args!(
                "Only the first statement of a SQLite query can have parameters"
            )));
        }
        let tail = stmt.tail_offset();
        let stmt = RefCell::new(stmt);
        if changes.is_none() && !bindings.is_undefined_or_null() {
            let map = RefCell::new(BindingsMap::new(internal_flags & STRICT_FLAG != 0));
            Binder {
                global,
                target: Target::Transient {
                    stmt: &stmt,
                    connection,
                },
                bindings: &map,
                safe_integers: internal_flags & SAFE_INTEGERS_FLAG != 0,
            }
            .bind(bindings, false)?;
        }
        let mut stmt = stmt.into_inner();
        let can_write = !stmt.get_program().is_readonly();
        let before = if can_write { conn.total_changes() } else { 0 };
        let ran = loop {
            match step(global, &mut stmt) {
                Ok(true) => continue,
                Ok(false) => break Ok(()),
                Err(error) => break Err(error),
            }
        };
        if let Err(error) = ran {
            return Err(SqliteError::from(error).at(head).throw(global));
        }
        changes = Some(changes.unwrap_or(0) + if can_write { changes_since(&conn, before) } else { 0 });
        if tail == 0 {
            break;
        }
        head += tail;
    }
    Ok(changes)
}

// ───────────────────────────── the constructor's functions ─────────────────────────────

fn open(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let path = frame.argument(0);
    if !path.is_string() {
        return Err(global.throw(format_args!("Expected string")));
    }
    let path = to_c_string(global, "filename", &path)?;
    let mut flags = DEFAULT_SQLITE_FLAGS;
    if frame.arguments_count() > 1 {
        let Some(number) = frame.argument(1).get_number() else {
            return Err(global.throw(format_args!("Expected number")));
        };
        flags = JSValue::js_number(number).coerce_to_i32(global)?;
    }
    match Connection::open(&path, flags) {
        Ok(connection) => database(global, connection),
        Err(error) => Err(error.throw(global)),
    }
}

fn close(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let connection = connection_for_handle(global, &frame.argument(0))?;
    let force = frame.argument(1).to_boolean();
    // A no-op if already closed; close(true) after close(false) still finalizes what was kept.
    if !connection.is_open() || (connection.is_closed() && !force) {
        return Ok(JSValue::UNDEFINED);
    }
    if let Some(database) = frame.argument(0).as_class_ref::<SQLDatabase>() {
        if force {
            database.borrow_mut(global)?.update_hooks = [None, None, None];
        }
    }
    match connection.close(force) {
        Ok(()) => Ok(JSValue::UNDEFINED),
        Err(error) => Err(global.throw(format_args!("{}", errstr(error.code)))),
    }
}

/// `(handle, sql, bindings, prepareFlags, internalFlags)`.
fn prepare(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let handle = frame.argument(0);
    let sql = frame.argument(1);
    let bindings = frame.argument(2);
    let prepare_flags = frame.argument(3);
    let internal_flags = frame.argument(4);
    let connection = connection_for_handle(global, &handle)?;
    let Some(sql) = sql.as_string() else {
        return Err(global.throw(format_args!("Expected SQL string")));
    };
    let Some(conn) = connection.handle() else {
        return Err(global.throw_range_error(format_args!("Cannot use a closed database")));
    };
    if sql.is_empty() {
        return Err(global.throw_range_error(format_args!("Invalid SQL statement")));
    }
    // Hints to SQLite's planner, which Turso has no use for: only checked.
    if let Some(number) = prepare_flags.get_number() {
        let flags = JSValue::js_number(number).coerce_to_i32(global)?;
        if !(0..=MAX_SQLITE_PREPARE_FLAG).contains(&flags) {
            return Err(global.throw_range_error(format_args!("Invalid prepare flags")));
        }
    }
    let stmt = match conn.prepare(up_to_nul(&sql)) {
        Ok(stmt) => stmt,
        // The input contained no SQL (only whitespace and/or comments).
        Err(error) if has_no_statement(&error) => {
            return Err(global.throw_range_error(format_args!("Invalid SQL statement")));
        }
        Err(error) => return Err(SqliteError::from(error).throw(global)),
    };
    let core = StatementCore::new(stmt, connection);
    if internal_flags.is_int32() {
        let flags = internal_flags.as_int32();
        core.bindings.borrow_mut().strict = flags & STRICT_FLAG != 0;
        core.use_big_int64.set(flags & SAFE_INTEGERS_FLAG != 0);
        core.owned_by_database.set(flags & OWNED_BY_DATABASE_FLAG != 0);
    }
    let statement = statement::create(global, core.clone(), handle)?;
    if !bindings.is_undefined() {
        let bound = Binder {
            global,
            target: Target::Statement(&core),
            bindings: &core.bindings,
            safe_integers: core.use_big_int64.get(),
        }
        .bind(&bindings, true);
        if let Err(error) = bound {
            // Nobody gets this statement. Left to the collector, it would keep the file open after db.close().
            core.finalize();
            return Err(error);
        }
    }
    Ok(statement)
}

/// db.run(): `(handle, internalFlags, tuple, sql, bindings)`.
fn run(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let connection = connection_for_handle(global, &frame.argument(0))?;
    let Some(conn) = connection.handle() else {
        return Err(global.throw(format_args!("Database has closed")));
    };
    let Some(sql) = frame.argument(3).as_string() else {
        return Err(global.throw(format_args!("Expected SQL string")));
    };
    if sql.is_empty() {
        return Err(global.throw(format_args!("SQL string mustn't be blank")));
    }
    let internal_flags = int32_flags(&frame.argument(1));
    let changes = run_statements(global, &connection, up_to_nul(&sql), 0, &frame.argument(4), internal_flags, false)?;
    let Some(changes) = changes else {
        return Err(global.throw(format_args!(
            "Query contained no valid SQL statement; likely empty query."
        )));
    };
    put_changes(global, &frame.argument(2), &conn, changes, internal_flags & SAFE_INTEGERS_FLAG != 0)?;
    Ok(JSValue::UNDEFINED)
}

fn is_in_transaction(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let connection = connection_for_handle(global, &frame.argument(0))?;
    let Some(conn) = connection.handle() else {
        return Err(global.throw(format_args!("Database has closed")));
    };
    Ok(JSValue::js_boolean(!conn.get_auto_commit()))
}

fn load_extension(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let connection = connection_for_handle(global, &frame.argument(0))?;
    let extension = frame.argument(1);
    if !extension.is_string() {
        return Err(global.throw_type_error(format_args!("Expected string")));
    }
    to_c_string(global, "extension", &extension)?;
    if frame.argument(2).is_string() {
        to_c_string(global, "entryPoint", &frame.argument(2))?;
    }
    if connection.handle().is_none() {
        return Err(global.throw(format_args!("Can't do this on a closed database")));
    }
    // Turso loads extensions of its own ABI, not SQLite's.
    Err(global.throw(format_args!(
        "This build of sqlite3 does not support dynamic extension loading"
    )))
}

/// SQLite is part of this binary: like Bun where it links SQLite statically, `false`.
fn set_custom_sqlite(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let path = frame.argument(0);
    if !path.is_string() {
        return Err(global.throw(format_args!("Expected SQLite path")));
    }
    to_c_string(global, "path", &path)?;
    Ok(JSValue::js_boolean(false))
}

static NEXT_SERIALIZED: AtomicU64 = AtomicU64::new(0);

/// `sqlite3_serialize`: what `VACUUM INTO` writes, which is the database as one file.
fn serialize(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let connection = connection_for_handle(global, &frame.argument(0))?;
    let name = to_c_string(global, "name", &frame.argument(1))?;
    if name.is_empty() {
        return Err(global.throw(format_args!("Expected attached database name")));
    }
    // Read after toCString: a user toString() may have closed the database.
    let Some(conn) = connection.handle() else {
        return Err(global.throw(format_args!("Can't do this on a closed database")));
    };
    let dir: &[u8] = bun_core::env_var::TMPDIR::get().unwrap_or(b"/tmp");
    let file = format!(
        "bun-sqlite-serialize-{}-{}.db",
        std::process::id(),
        NEXT_SERIALIZED.fetch_add(1, Ordering::Relaxed)
    );
    let path = bun_paths::Path::from_parts(&[dir, file.as_bytes()]);
    let path_text = String::from_utf8_lossy(path.as_bytes()).into_owned();
    let sql = format!(
        "VACUUM \"{}\" INTO '{}'",
        name.replace('"', "\"\""),
        path_text.replace('\'', "''")
    );
    let vacuumed = conn.execute(&sql);
    let bytes = vacuumed.map_err(SqliteError::from).and_then(|()| {
        bun_sys::File::read_from(bun_sys::Fd::cwd(), path.as_bytes())
            .map_err(|error| SqliteError::new(10, format!("{error}")))
    });
    for suffix in ["", "-wal", "-shm"] {
        let mut leftover = bun_paths::Path::from(path.as_bytes());
        leftover.extend_from_slice(suffix.as_bytes());
        let _ = bun_sys::unlink(&leftover);
    }
    let bytes = bytes.map_err(|error| error.throw(global))?;
    let array = engine::uint8_array(global, &bytes)?;
    // A Buffer, as Bun's, where node:buffer is there: a Uint8Array whose prototype is Buffer.prototype.
    if let Some(prototype) = global
        .to_js_value()
        .get(global, "Buffer")?
        .map(|buffer| buffer.get(global, "prototype"))
        .transpose()?
        .flatten()
    {
        engine::derive_prototype(&array, &prototype);
    }
    Ok(array)
}

/// `(bytes, flags)`.
fn deserialize(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let array = frame.argument(0);
    let flags = frame.argument(1).coerce_to_i32(global)?;
    let bytes = match engine::bytes_of(global, &array, true)? {
        Bytes::Bytes(bytes) => bytes,
        Bytes::Detached => return Err(global.throw(format_args!("TypedArray is detached"))),
        Bytes::Other => return Err(global.throw(format_args!("Expected Uint8Array or Buffer"))),
    };
    if bytes.is_empty() {
        return Err(global.throw(format_args!("ArrayBuffer must not be empty")));
    }
    match Connection::deserialize(bytes, flags) {
        Ok(connection) => database(global, connection),
        Err(error) => Err(global.throw(format_args!("{}", error.message))),
    }
}

/// `(handle, name, op, result)`. Turso has no `sqlite3_file_control`: its VFS knows no opcode, which SQLite answers
/// with `SQLITE_NOTFOUND`.
fn fcntl(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let name = frame.argument(1);
    let op = frame.argument(2);
    let result = frame.argument(3);
    let connection = connection_for_handle(global, &frame.argument(0))?;
    if !op.is_number() {
        return Err(global.throw(format_args!("Expected number")));
    }
    // A no-op if already closed.
    if connection.handle().is_none() {
        return Ok(JSValue::UNDEFINED);
    }
    if name.is_string() {
        to_c_string(global, "zDbName", &name)?;
    }
    if !result.is_number() && !result.is_undefined_or_null() {
        match engine::bytes_of(global, &result, true)? {
            Bytes::Detached => return Err(global.throw(format_args!("TypedArray is detached"))),
            Bytes::Bytes(bytes) if bytes.len() < 8 => {
                return Err(global.throw(format_args!("TypedArray must be at least 8 bytes")));
            }
            Bytes::Bytes(_) => {}
            Bytes::Other => {
                return Err(global.throw(format_args!(
                    "Expected result to be a number, null or a TypedArray"
                )));
            }
        }
    }
    Ok(JSValue::js_number(f64::from(SQLITE_NOTFOUND)))
}

fn handle_number(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let connection = connection_for_handle(global, &frame.argument(0))?;
    Ok(JSValue::js_number(connection.handle_number as f64))
}

/// db.function(): `(handle, name, argumentCount, flags, safeIntegers, function)`; db.aggregate(): `(handle, name,
/// argumentCount, flags, safeIntegers, start, step, inverse, result)`.
fn define_function(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let connection = connection_for_handle(global, &frame.argument(0))?;
    let is_aggregate = frame.arguments_count() > 6;
    let function = frame.argument(if is_aggregate { 6 } else { 5 });
    if !frame.argument(1).is_string()
        || !frame.argument(2).is_int32()
        || !frame.argument(3).is_int32()
        || !function.is_callable()
    {
        return Err(global.throw_type_error(format_args!(
            "Expected a name, two integers and a function"
        )));
    }
    let name = to_c_string(global, "name", &frame.argument(1))?;
    let argument_count = frame.argument(2).as_int32();
    let deterministic = frame.argument(3).as_int32() & SQLITE_DETERMINISTIC != 0;
    let Some(conn) = connection.handle() else {
        return Err(global.throw(format_args!("Database has closed")));
    };
    let callback = |index: usize| {
        let value = frame.argument(index);
        value.is_callable().then_some(value)
    };
    let kind = if is_aggregate {
        function::Kind::Aggregate {
            start: frame.argument(5),
            step: function,
            result: callback(8),
        }
    } else {
        function::Kind::Scalar(function)
    };
    let safe_integers = frame.argument(4).is_boolean() && frame.argument(4).as_boolean();
    match function::define(&connection, &conn, &name, argument_count, deterministic, safe_integers, kind) {
        Ok(()) => Ok(JSValue::UNDEFINED),
        Err(error) => Err(error.throw(global)),
    }
}

fn update_hook_index(operation: &JSValue) -> Option<usize> {
    if !operation.is_int32() {
        return None;
    }
    match operation.as_int32() {
        SQLITE_INSERT => Some(0),
        SQLITE_UPDATE => Some(1),
        SQLITE_DELETE => Some(2),
        _ => None,
    }
}

fn update_hook_of(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<usize> {
    connection_for_handle(global, &frame.argument(0))?;
    update_hook_index(&frame.argument(1)).ok_or_else(|| {
        global.throw_type_error(format_args!(
            "Expected SQLITE_INSERT, SQLITE_UPDATE or SQLITE_DELETE"
        ))
    })
}

fn get_update_hook(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let index = update_hook_of(global, frame)?;
    let database = frame.argument(0).as_class_ref::<SQLDatabase>().expect("checked");
    Ok(database.borrow(global)?.update_hooks[index].clone().unwrap_or(JSValue::NULL))
}

/// `(handle, operation, hook, database, internalFlags)`. Turso calls no hook when a row changes: setting one throws.
fn set_update_hook(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let index = update_hook_of(global, frame)?;
    let hook = frame.argument(2);
    if !hook.is_undefined_or_null() && !hook.is_callable() {
        return Err(global.throw_type_error(format_args!("Expected a function or null")));
    }
    let database = frame.argument(0).as_class_ref::<SQLDatabase>().expect("checked");
    if database.borrow(global)?.connection.handle().is_none() {
        return Err(global.throw(format_args!("Database has closed")));
    }
    if hook.is_callable() {
        return Err(global.throw(format_args!(
            "Update hooks are not supported: bun:sqlite on Boa runs on Turso, which has no sqlite3_update_hook"
        )));
    }
    database.borrow_mut(global)?.update_hooks[index] = None;
    Ok(JSValue::UNDEFINED)
}

/// The functions on `SQLStatement`, the constructor.
pub fn define_statics(class: &mut ClassBuilder<'_, SQLStatement>) -> JsResult<()> {
    let function = Attributes {
        writable: true,
        enumerable: true,
        configurable: true,
    };
    for (name, length, f) in [
        ("open", 2, open as bun_js::JSHostFn),
        ("close", 1, close),
        ("prepare", 2, prepare),
        ("run", 3, run),
        ("isInTransaction", 1, is_in_transaction),
        ("loadExtension", 2, load_extension),
        ("setCustomSQLite", 1, set_custom_sqlite),
        ("serialize", 1, serialize),
        ("deserialize", 2, deserialize),
        ("fcntl", 2, fcntl),
        ("handleNumber", 1, handle_number),
        ("defineFunction", 6, define_function),
        ("updateHook", 2, get_update_hook),
        ("setUpdateHook", 5, set_update_hook),
    ] {
        class.static_method(name, length, function, f)?;
    }
    Ok(())
}

/// `createJSSQLStatementConstructor`: `{ 0: SQL, 1: tuple }`, the tuple's internal fields what a run changed.
pub fn create_constructor(global: &JSGlobalObject) -> JsResult<JSValue> {
    let constructor = bun_js::class::constructor::<SQLStatement>(global)?;
    let tuple = bun_js::JSObject::create(global);
    tuple.put_internal_field(global, 0, JSValue::UNDEFINED)?;
    tuple.put_internal_field(global, 1, JSValue::UNDEFINED)?;
    let object = bun_js::JSObject::create(global);
    object.put(global, "0", constructor);
    object.put(global, "1", tuple.to_js());
    Ok(object.to_js())
}
