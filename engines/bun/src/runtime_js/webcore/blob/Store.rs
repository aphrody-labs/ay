//! `Blob.Store`: what a `Blob` views, bytes in memory or a file by path or descriptor. The data types of
//! src/jsc/webcore_types.rs, and the reads of src/runtime/webcore/blob/read_file.rs, run where they are asked for:
//! there is no thread pool on Boa yet.

use core::cell::Cell;
use std::rc::Rc;

use bun_http_types::MimeType::{self as mime_type, MimeType};
use bun_paths::Path;
use bun_sys::{Fd, Mode, O, S};

use crate::sys_js::SystemError;

/// Blob size type. Values are masked to 52 bits at the boundary.
pub type SizeType = u64;
/// Maximum blob size (52 bits); a `Blob` of this size has not learnt its size yet.
pub const MAX_SIZE: SizeType = (1 << 52) - 1;
/// `lastModified` before a stat answers it (`jsc::INIT_TIMESTAMP`).
pub const INIT_TIMESTAMP: u64 = (1 << 52) - 1;

/// Milliseconds since the epoch, as `jsc::to_js_time`.
pub fn to_js_time(sec: i64, nsec: i64) -> u64 {
    let total = i128::from(sec) * 1000 + i128::from(nsec) / 1_000_000;
    (total as u64) & ((1u64 << 52) - 1)
}

#[derive(Clone, Debug)]
pub enum PathOrFileDescriptor {
    Path(Vec<u8>),
    Fd(Fd),
}

impl PathOrFileDescriptor {
    pub fn path(&self) -> Option<&[u8]> {
        match self {
            PathOrFileDescriptor::Path(path) => Some(path),
            PathOrFileDescriptor::Fd(_) => None,
        }
    }
}

/// A file on disk. What a stat learnt is kept: `size`, `exists()` and `lastModified` answer from it.
pub struct File {
    pub pathlike: PathOrFileDescriptor,
    pub mime_type: MimeType,
    pub mode: Cell<Mode>,
    pub seekable: Cell<Option<bool>>,
    pub max_size: Cell<SizeType>,
    /// Milliseconds since the epoch; [`INIT_TIMESTAMP`] until a stat or a read answers it.
    pub last_modified: Cell<u64>,
}

pub enum Data {
    Bytes(Vec<u8>),
    File(File),
}

pub struct Store {
    pub data: Data,
}

/// The MIME type of a path's extension, if it has a known one.
fn mime_from_path_ext(path: &[u8]) -> Option<MimeType> {
    if path.is_empty() {
        return None;
    }
    let mut ext = bun_paths::extension(path);
    while let [b'.', rest @ ..] = ext {
        ext = rest;
    }
    mime_type::by_extension_no_default(ext)
}

impl Store {
    pub fn init(bytes: Vec<u8>) -> Rc<Store> {
        Rc::new(Store {
            data: Data::Bytes(bytes),
        })
    }

    pub fn init_file(pathlike: PathOrFileDescriptor, mime: Option<MimeType>) -> Rc<Store> {
        let mime = mime.or_else(|| pathlike.path().and_then(mime_from_path_ext));
        Rc::new(Store {
            data: Data::File(File {
                pathlike,
                mime_type: mime.unwrap_or(mime_type::OTHER),
                mode: Cell::new(0),
                seekable: Cell::new(None),
                max_size: Cell::new(MAX_SIZE),
                last_modified: Cell::new(INIT_TIMESTAMP),
            }),
        })
    }

    pub fn size(&self) -> SizeType {
        match &self.data {
            Data::Bytes(bytes) => bytes.len() as SizeType,
            Data::File(file) => file.max_size.get(),
        }
    }

    pub fn as_file(&self) -> Option<&File> {
        match &self.data {
            Data::File(file) => Some(file),
            Data::Bytes(_) => None,
        }
    }
}

/// A descriptor this read or write opened, closed on drop, or the one the `Blob` was made of.
pub(crate) enum Opened {
    Owned(bun_sys::File),
    Borrowed(Fd),
}

impl Opened {
    pub(crate) fn fd(&self) -> Fd {
        match self {
            Opened::Owned(file) => file.fd(),
            Opened::Borrowed(fd) => *fd,
        }
    }
}

/// Waits until `fd` can be read or written, for a descriptor that is non-blocking.
pub(crate) fn wait_for(fd: Fd, events: i16) {
    let mut fds = [libc::pollfd {
        fd: fd.native(),
        events,
        revents: 0,
    }];
    let _ = bun_core::util::poll_once(&mut fds, -1);
}

impl File {
    /// `resolve_file_stat`: the size, the mode and `lastModified`. A file that does not exist yet leaves them unknown.
    pub fn resolve_stat(&self) {
        let stat = match &self.pathlike {
            PathOrFileDescriptor::Path(path) => bun_sys::stat(&Path::from(path.as_slice())),
            PathOrFileDescriptor::Fd(fd) => bun_sys::fstat(*fd),
        };
        if let Ok(stat) = stat {
            self.record(&stat);
        }
    }

    fn record(&self, stat: &bun_sys::Stat) {
        let mode = stat.st_mode as Mode;
        self.max_size.set(if S::ISREG(mode) || stat.st_size > 0 {
            stat.st_size.max(0) as SizeType
        } else {
            MAX_SIZE
        });
        self.mode.set(mode);
        self.seekable.set(Some(S::ISREG(mode)));
        self.record_mtime(stat);
    }

    fn record_mtime(&self, stat: &bun_sys::Stat) {
        let mtime = bun_sys::PosixStat::init(stat).mtime();
        self.last_modified.set(to_js_time(mtime.sec, mtime.nsec));
    }

    /// Opens the file to read it: a path each time, a descriptor as it is.
    pub(crate) fn open_for_read(&self) -> Result<Opened, SystemError> {
        open_for_read(&self.pathlike)
    }

    /// `ReadFile`: up to `max_length` bytes from `offset`. A directory is an `EISDIR` error, as in Bun.
    pub fn read(&self, offset: SizeType, max_length: SizeType) -> Result<Vec<u8>, SystemError> {
        let read = read(&self.pathlike, offset, max_length)?;
        self.last_modified.set(read.last_modified);
        Ok(read.bytes)
    }
}

/// What a read of a file gave: its bytes, and the file's `lastModified`.
pub(crate) struct Read {
    pub bytes: Vec<u8>,
    pub last_modified: u64,
}

fn open_for_read(pathlike: &PathOrFileDescriptor) -> Result<Opened, SystemError> {
    match pathlike {
        PathOrFileDescriptor::Fd(fd) => Ok(Opened::Borrowed(*fd)),
        PathOrFileDescriptor::Path(path) => bun_sys::File::open(&Path::from(path.as_slice()), O::RDONLY | O::CLOEXEC, 0)
            .map(Opened::Owned)
            .map_err(|err| SystemError::from(err.with_path(path))),
    }
}

/// `ReadFile` of a path or a descriptor, on any thread: up to `max_length` bytes from `offset`.
pub(crate) fn read(pathlike: &PathOrFileDescriptor, offset: SizeType, max_length: SizeType) -> Result<Read, SystemError> {
    let error_path = pathlike.path().unwrap_or_default();
    {
        let opened = open_for_read(pathlike)?;
        let fd = opened.fd();
        let stat = bun_sys::fstat(fd)
            .map_err(|err| SystemError::from(err).with_path(error_path))?;
        let mtime = bun_sys::PosixStat::init(&stat).mtime();
        let last_modified = to_js_time(mtime.sec, mtime.nsec);
        if S::ISDIR(stat.st_mode as Mode) {
            return Err(SystemError {
                code: Some("EISDIR"),
                path: pathlike.path().map(<[u8]>::to_vec),
                message: "Directories cannot be read like files".to_owned(),
                syscall: Some("read"),
                ..SystemError::default()
            });
        }
        let regular = bun_sys::is_regular_file(stat.st_mode as Mode);
        if offset > 0 {
            // A pipe cannot seek, and is read anyway.
            let _ = bun_sys::set_file_offset(fd, offset);
        }
        let mut buffer = Vec::new();
        if regular && stat.st_size > 0 {
            let expected = (stat.st_size as SizeType).min(MAX_SIZE).min(max_length);
            buffer.reserve_exact(expected as usize);
        }
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let max = (max_length as usize).saturating_sub(buffer.len());
            if max == 0 {
                break;
            }
            let len = chunk.len().min(max);
            match bun_sys::read(fd, &mut chunk[..len]) {
                Ok(0) => break,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                Err(err) if err.get_errno() == bun_sys::E::EAGAIN => wait_for(fd, libc::POLLIN),
                Err(err) => return Err(SystemError::from(err).with_path(error_path)),
            }
        }
        Ok(Read {
            bytes: buffer,
            last_modified,
        })
    }
}

/// Creates the parent directories of `path`.
pub(crate) fn mkdirp_parent(path: &[u8]) -> bun_sys::Result<()> {
    let Some(dirname) = bun_paths::dirname(path) else {
        return Err(bun_sys::Error::from_code(
            bun_sys::E::ENOENT,
            bun_sys::Tag::mkdir,
        ));
    };
    bun_sys::mkdir_recursive(dirname)
}

/// Runs `create` again if the parents of `path` were missing and could be made, which clears `mkdirp`.
pub(crate) fn creating_parents<T>(
    mkdirp: &mut bool,
    path: &[u8],
    create: impl Fn() -> bun_sys::Result<T>,
) -> bun_sys::Result<T> {
    match create() {
        Err(err)
            if *mkdirp && err.get_errno() == bun_sys::E::ENOENT && mkdirp_parent(path).is_ok() =>
        {
            *mkdirp = false;
            create()
        }
        result => result,
    }
}
