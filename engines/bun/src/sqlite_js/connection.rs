//! One connection, as JSSQLStatement.cpp's `VersionSqlite3`: the Database's handle and each of its statements hold
//! it, and the connection to the engine closes when it is closed and its last statement is finalized.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use turso_core::{
    Buffer, Completion, CompletionError, LimboError, ConnectOpts, Database, DatabaseOpts, IO, MemoryIO, OpenFlags,
    PlatformIO, SessionRole,
};
use turso_sqlite::SqliteDialect;

use crate::error::{Result, SQLITE_CANTOPEN, SqliteError, errstr};
use crate::statement::StatementCore;

pub const SQLITE_OPEN_READONLY: i32 = 0x1;
pub const SQLITE_OPEN_CREATE: i32 = 0x4;
pub const SQLITE_OPEN_MEMORY: i32 = 0x80;
pub const SQLITE_DESERIALIZE_READONLY: i32 = 0x4;

/// What SQLite has and Turso keeps behind a flag.
fn sqlite_features() -> DatabaseOpts {
    DatabaseOpts::new()
        .with_views(true)
        .with_vacuum(true)
        .with_autovacuum(true)
        .with_attach(true)
        .with_without_rowid(true)
}

/// The engine's side of an open connection.
pub struct Open {
    _db: Arc<Database>,
    pub conn: Arc<turso_core::Connection>,
}

pub struct Connection {
    open: RefCell<Option<Open>>,
    /// `close(false)` with statements of `db.prepare()` alive: closed for script, the engine's connection kept until
    /// they are finalized.
    closed: Cell<bool>,
    /// Script runs from inside a statement of this connection (a column name transform), so every call on the
    /// connection throws.
    pub in_callback: Cell<bool>,
    statements: RefCell<Vec<Weak<StatementCore>>>,
    /// `db.handle`.
    pub handle_number: u64,
}

static NEXT_HANDLE_NUMBER: AtomicU64 = AtomicU64::new(0);

impl Connection {
    fn new(open: Open) -> Rc<Connection> {
        EXIT_SENTINEL.with(|_| {});
        Rc::new(Connection {
            open: RefCell::new(Some(open)),
            closed: Cell::new(false),
            in_callback: Cell::new(false),
            statements: RefCell::new(Vec::new()),
            handle_number: NEXT_HANDLE_NUMBER.fetch_add(1, Ordering::Relaxed),
        })
    }

    fn connect(db: Arc<Database>) -> Result<Rc<Connection>> {
        let conn = db.connect(ConnectOpts::new(SessionRole::Superuser))?;
        Ok(Connection::new(Open { _db: db, conn }))
    }

    /// `sqlite3_open_v2(path, flags)`.
    pub fn open(path: &str, flags: i32) -> Result<Rc<Connection>> {
        let mut open_flags = if flags & SQLITE_OPEN_READONLY != 0 {
            OpenFlags::ReadOnly
        } else {
            OpenFlags::None
        };
        if flags & SQLITE_OPEN_CREATE != 0 {
            open_flags |= OpenFlags::Create;
        }
        let memory = path == ":memory:" || flags & SQLITE_OPEN_MEMORY != 0;
        let io: Arc<dyn IO> = if memory {
            Arc::new(MemoryIO::new())
        } else {
            Arc::new(PlatformIO::new()?)
        };
        let path = if memory { ":memory:" } else { path };
        let db = Database::open_file_with_flags(
            io,
            path,
            open_flags,
            sqlite_features(),
            None,
            Arc::new(SqliteDialect),
        )
        .map_err(|error| match error {
            // A missing directory fails in Turso where the WAL's coordination file is looked for, before the open.
            LimboError::CompletionError(CompletionError::IOError(..)) => {
                SqliteError::new(SQLITE_CANTOPEN, errstr(SQLITE_CANTOPEN))
            }
            error => SqliteError::from(error),
        })?;
        Connection::connect(db)
    }

    /// `sqlite3_deserialize` into a connection of its own: an in-memory database whose file is `bytes`.
    pub fn deserialize(bytes: Vec<u8>, flags: i32) -> Result<Rc<Connection>> {
        const PATH: &str = ":memory:deserialized";
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let file = io.open_file(PATH, OpenFlags::Create, false)?;
        let written = file.pwrite(
            0,
            Arc::new(Buffer::new(bytes)),
            Completion::new_write(|_| {}),
        )?;
        io.wait_for_completion(written)?;
        drop(file);
        // A database in WAL mode (what Turso serializes) opens its WAL too, which `flags` would not create.
        io.open_file(&format!("{PATH}-wal"), OpenFlags::Create, false)?;
        let flags = if flags & SQLITE_DESERIALIZE_READONLY != 0 {
            OpenFlags::ReadOnly
        } else {
            OpenFlags::None
        };
        let db = Database::open_file_with_flags(
            io,
            PATH,
            flags,
            sqlite_features(),
            None,
            Arc::new(SqliteDialect),
        )
        .map_err(|_| SqliteError::new(crate::error::SQLITE_ERROR, "unable to deserialize database"))?;
        Connection::connect(db)
    }

    /// The engine's connection, `None` once script closed it.
    pub fn handle(&self) -> Option<Arc<turso_core::Connection>> {
        if self.closed.get() {
            return None;
        }
        self.engine()
    }

    /// The engine's connection while it is open, closed for script or not: what statements still run on.
    pub fn engine(&self) -> Option<Arc<turso_core::Connection>> {
        self.open.borrow().as_ref().map(|open| open.conn.clone())
    }

    pub fn is_open(&self) -> bool {
        self.open.borrow().is_some()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub fn add_statement(&self, statement: &Rc<StatementCore>) {
        let mut statements = self.statements.borrow_mut();
        statements.retain(|statement| statement.strong_count() > 0);
        statements.push(Rc::downgrade(statement));
    }

    fn live_statements(&self) -> Vec<Rc<StatementCore>> {
        self.statements
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    /// `db.close(force)`. With `force` every statement is finalized and the engine's connection closes now; without
    /// it the statements of `db.prepare()` stay usable, and it closes when they are finalized.
    pub fn close(&self, force: bool) -> Result<()> {
        for statement in self.live_statements() {
            if force || statement.owned_by_database.get() {
                statement.finalize_by_close();
            }
        }
        self.closed.set(true);
        if !force {
            self.close_if_drained();
            return Ok(());
        }
        self.close_engine()
    }

    fn close_engine(&self) -> Result<()> {
        let Some(open) = self.open.borrow_mut().take() else {
            return Ok(());
        };
        open.conn.close()?;
        Ok(())
    }

    /// Closes the engine's connection once script closed it and no statement is left.
    pub fn close_if_drained(&self) {
        if !self.closed.get() || !self.is_open() {
            return;
        }
        if self
            .live_statements()
            .iter()
            .any(|statement| statement.is_prepared())
        {
            return;
        }
        let _ = self.close_engine();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let Some(open) = self.open.get_mut().take() else {
            return;
        };
        if thread_is_exiting() {
            let _ = std::mem::ManuallyDrop::new(open);
            return;
        }
        let _ = open.conn.close();
    }
}

thread_local! {
    static EXITING: Cell<bool> = const { Cell::new(false) };
    static EXIT_SENTINEL: ExitSentinel = const { ExitSentinel };
}

struct ExitSentinel;

impl Drop for ExitSentinel {
    fn drop(&mut self) {
        EXITING.set(true);
    }
}

/// Whether this thread's thread-locals are being destroyed. Boa's heap is one of them, and the objects it drops
/// then would drop the engine's pages into Turso's buffer cache, a thread-local that is gone: they are leaked
/// instead. The sentinel is touched once a connection opens, after Boa's heap exists, so it is destroyed before it.
pub fn thread_is_exiting() -> bool {
    EXITING.get()
}
