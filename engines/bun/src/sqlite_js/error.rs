//! `SQLiteError`, as JSSQLStatement.cpp's `createSQLiteError` makes it: an `Error` named "SQLiteError" with the
//! name of SQLite's result code in `code`, the code in `errno`, and where the SQL is wrong in `byteOffset`.

use bun_js::{JSGlobalObject, JSValue, JsError};
use turso_core::{CompletionError, LimboError};

use crate::engine;

include!(concat!(env!("OUT_DIR"), "/error_codes.rs"));

pub const SQLITE_ERROR: i32 = 1;
pub const SQLITE_BUSY: i32 = 5;
pub const SQLITE_CANTOPEN: i32 = 14;

/// A failure of the engine, as SQLite reports one: its extended result code, `sqlite3_errmsg` and
/// `sqlite3_error_offset` (-1 where there is none).
#[derive(Debug)]
pub struct SqliteError {
    pub code: i32,
    pub message: String,
    pub byte_offset: i64,
}

impl SqliteError {
    pub fn new(code: i32, message: impl Into<String>) -> SqliteError {
        SqliteError {
            code,
            message: message.into(),
            byte_offset: -1,
        }
    }

    /// The error's offset counts from `base` bytes into the caller's SQL.
    pub fn at(mut self, base: usize) -> SqliteError {
        if self.byte_offset >= 0 {
            self.byte_offset += base as i64;
        }
        self
    }

    /// What a user function threw, if one did during the step that failed, or this error.
    pub fn throw(&self, global: &JSGlobalObject) -> JsError {
        let scope = bun_js::TopExceptionScope::new(global);
        if scope.has_exception() {
            return scope.err_for_pending();
        }
        let error = global.create_error_instance(format_args!("{}", self.message));
        let Some(object) = error.get_object() else {
            return global.throw_value(error);
        };
        let defined = (|| {
            engine::define(
                global,
                &object,
                "name",
                JSValue::from_utf8(b"SQLiteError"),
                true,
                false,
                true,
            )?;
            if let Some(name) = name_of(self.code) {
                engine::define(
                    global,
                    &object,
                    "code",
                    JSValue::from_utf8(name.as_bytes()),
                    false,
                    true,
                    false,
                )?;
            }
            engine::define(
                global,
                &object,
                "errno",
                JSValue::js_number(f64::from(self.code)),
                false,
                true,
                false,
            )?;
            engine::define(
                global,
                &object,
                "byteOffset",
                JSValue::js_number(self.byte_offset as f64),
                false,
                true,
                false,
            )
        })();
        match defined {
            Ok(()) => global.throw_value(error),
            Err(error) => error,
        }
    }
}

/// `SQLITE_CONSTRAINT_UNIQUE` for 2067.
pub fn name_of(code: i32) -> Option<&'static str> {
    CODES
        .iter()
        .find_map(|&(value, name)| (value == code).then_some(name))
}

/// `sqlite3_errstr`.
pub fn errstr(code: i32) -> &'static str {
    match code & 0xff {
        0 => "not an error",
        1 => "SQL logic error",
        3 => "access permission denied",
        4 => "query aborted",
        5 => "database is locked",
        6 => "database table is locked",
        7 => "out of memory",
        8 => "attempt to write a readonly database",
        9 => "interrupted",
        10 => "disk I/O error",
        11 => "database disk image is malformed",
        12 => "unknown operation",
        13 => "database or disk is full",
        14 => "unable to open database file",
        15 => "locking protocol",
        17 => "database schema has changed",
        18 => "string or blob too big",
        19 => "constraint failed",
        20 => "datatype mismatch",
        21 => "bad parameter or other API misuse",
        23 => "authorization denied",
        25 => "column index out of range",
        26 => "file is not a database",
        _ => "unknown error",
    }
}

impl From<LimboError> for SqliteError {
    fn from(error: LimboError) -> SqliteError {
        let code = match &error {
            LimboError::CompletionError(CompletionError::IOError(_, "open")) => SQLITE_CANTOPEN,
            LimboError::StatementsInProgress(_) => SQLITE_BUSY,
            error => error.sqlite_extended_result_code(),
        };
        // Where Turso's text is its own, SQLite's for the code.
        let message = match &error {
            LimboError::Busy | LimboError::BusySnapshot => errstr(SQLITE_BUSY).to_owned(),
            _ if code == SQLITE_CANTOPEN => errstr(SQLITE_CANTOPEN).to_owned(),
            LimboError::LexerError(error) => match illegal_token(error) {
                Some(token) => format!("unrecognized token: \"{token}\""),
                None => error.to_string(),
            },
            error => error.to_string(),
        };
        let byte_offset = match &error {
            LimboError::LexerError(error) => offset_of(error),
            _ => -1,
        };
        SqliteError {
            code,
            message,
            byte_offset,
        }
    }
}

/// The text of a token that SQLite's tokenizer calls `TK_ILLEGAL`, where Turso's lexer names what is wrong with it.
fn illegal_token(error: &turso_parser::error::Error) -> Option<&str> {
    use turso_parser::error::Error;
    match error {
        Error::UnrecognizedToken { token_text, .. }
        | Error::UnterminatedLiteral { token_text, .. }
        | Error::UnterminatedBracket { token_text, .. }
        | Error::BadNumber { token_text, .. }
        | Error::BadFractionalPart { token_text, .. }
        | Error::BadExponentPart { token_text, .. }
        | Error::ExpectedEqualsSign { token_text, .. }
        | Error::MalformedHexInteger { token_text, .. } => Some(token_text),
        _ => None,
    }
}

fn offset_of(error: &turso_parser::error::Error) -> i64 {
    use turso_parser::error::Error;
    match error {
        Error::UnrecognizedToken { offset, .. }
        | Error::UnterminatedLiteral { offset, .. }
        | Error::UnterminatedBracket { offset, .. }
        | Error::UnterminatedBlockComment { offset, .. }
        | Error::BadNumber { offset, .. }
        | Error::BadFractionalPart { offset, .. }
        | Error::BadExponentPart { offset, .. }
        | Error::ExpectedEqualsSign { offset, .. }
        | Error::MalformedHexInteger { offset, .. }
        | Error::ParseUnexpectedToken { offset, .. } => *offset as i64,
        _ => -1,
    }
}

pub type Result<T, E = SqliteError> = core::result::Result<T, E>;
