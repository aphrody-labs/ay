//! The operations of node:fs on `bun_js`: src/runtime/node/node_fs.rs, whose `args::*::from_js` parse what
//! JavaScript passes, `NodeFS::<op>` does the system calls and `FsReturn::fs_to_js` makes the result, over the same
//! `bun_sys`.
//!
//! An operation is an [`Operation`]: what `from_js` parses is its own (paths and bytes are copied out of their values)
//! and goes to the work pool for the async forms, while what stays on the JavaScript thread (the buffer `read` fills)
//! is its `Keep`.

use bun_core::{ZBox, ZStr, strings};
use bun_js::{JSArray, JSGlobalObject, JSObject, JSValue, JsResult};
use bun_sys::{E, Fd, Maybe, Mode, O, PosixStat};

use crate::node::stat;
use crate::node::types::{
    self, ArgumentsSlice, Encoding, PathLike, PathOrFd, fd_required, invalid_arg_type,
    invalid_arg_value, invalid_arguments, mode_from_js, out_of_range, path_like_from_js, path_like_required,
    path_or_fd_from_js, validate_integer,
};

/// `DEFAULT_PERMISSION` of node_fs.rs: what `open` and `writeFile` create a file with.
const DEFAULT_PERMISSION: Mode = 0o666;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// One operation of node:fs, as an `args::*` of node_fs.rs and the `NodeFS` method that runs it.
pub(crate) trait Operation: Sized + Send + 'static {
    /// What the system calls produce, which comes back to the JavaScript thread.
    type Out: Send + 'static;
    /// What the call keeps on the JavaScript thread until the result is made.
    type Keep: 'static;
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, Self::Keep)>;
    fn run(&self) -> Maybe<Self::Out>;
    fn to_js(&self, global: &JSGlobalObject, out: Self::Out, keep: Self::Keep) -> JsResult<JSValue>;
}

/// An operation whose `Keep` is nothing: what most are.
macro_rules! parse_only {
    () => {
        type Keep = ();
        fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, ())> {
            Self::parse(global, arguments).map(|op| (op, ()))
        }
    };
}

fn z(path: &PathLike) -> &ZStr {
    path.as_zstr()
}

fn undefined(_: ()) -> JsResult<JSValue> {
    Ok(JSValue::UNDEFINED)
}

/// `{ bigint }` of an options object that is not a function, which is taken.
fn bigint_option(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<bool> {
    if let Some(next) = arguments.next() {
        if next.is_object() && !next.is_callable() {
            arguments.eat();
            return Ok(boolean_strict(global, &next, "bigint")?.unwrap_or(false));
        }
    }
    Ok(false)
}

/// `get_boolean_strict`: `None` where it is absent or `undefined`; anything but a boolean throws.
fn boolean_strict(global: &JSGlobalObject, options: &JSValue, key: &str) -> JsResult<Option<bool>> {
    match options.get(global, key)? {
        None => Ok(None),
        Some(value) if value.is_boolean() => Ok(Some(value.as_boolean())),
        Some(value) => {
            let actual = value.type_name(global);
            Err(bun_js::ErrorCode::INVALID_ARG_TYPE.throw(
                global,
                format_args!("The \"{key}\" property must be of type boolean, got {actual}"),
            ))
        }
    }
}

/// `parse_encoding_arg`: a string, or an object's `encoding`.
fn encoding_arg(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>, default: Encoding) -> JsResult<Encoding> {
    let Some(value) = arguments.next_eat() else {
        return Ok(default);
    };
    if value.is_string() || value.is_string_object() {
        return Encoding::assert(global, &value.to_js_string(global)?, default);
    }
    if value.is_object() {
        if let Some(encoding) = value.get(global, "encoding")? {
            return Encoding::assert(global, &encoding, default);
        }
    }
    Ok(default)
}

fn path_in(global: &JSGlobalObject, bytes: &[u8], encoding: Encoding) -> JsResult<JSValue> {
    encoding.to_js(global, bytes)
}

// ───────────────────────────── stat ─────────────────────────────

fn posix_stat(stat: &bun_sys::Stat) -> PosixStat {
    PosixStat::init(stat)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn statx_supported() -> bool {
    bun_sys::SUPPORTS_STATX_ON_LINUX.load(core::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn stat_path(path: &ZStr, follow: bool) -> Maybe<PosixStat> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if statx_supported() {
        return if follow {
            bun_sys::statx(path, bun_sys::STATX_MASK_FOR_STATS)
        } else {
            bun_sys::lstatx(path, bun_sys::STATX_MASK_FOR_STATS)
        };
    }
    let stat = if follow { bun_sys::stat(path) } else { bun_sys::lstat(path) }?;
    Ok(posix_stat(&stat))
}

pub(crate) struct Stat {
    path: PathLike,
    big_int: bool,
    throw_if_no_entry: bool,
    follow: bool,
}

impl Stat {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>, follow: bool) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let mut throw_if_no_entry = true;
        let mut big_int = false;
        if let Some(next) = arguments.next() {
            if next.is_object() && !next.is_callable() {
                arguments.eat();
                if let Some(value) = boolean_strict(global, &next, "throwIfNoEntry")? {
                    throw_if_no_entry = value;
                }
                big_int = boolean_strict(global, &next, "bigint")?.unwrap_or(false);
            }
        }
        Ok(Stat {
            path,
            big_int,
            throw_if_no_entry,
            follow,
        })
    }
}

pub(crate) struct StatOp<const FOLLOW: bool>(Stat);

impl<const FOLLOW: bool> Operation for StatOp<FOLLOW> {
    type Out = Option<PosixStat>;
    type Keep = ();
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, ())> {
        Ok((StatOp(Stat::parse(global, arguments, FOLLOW)?), ()))
    }
    fn run(&self) -> Maybe<Self::Out> {
        let args = &self.0;
        match stat_path(z(&args.path), args.follow) {
            Ok(stat) => Ok(Some(stat)),
            Err(err) if err.get_errno() == E::ENOENT && !args.throw_if_no_entry => Ok(None),
            Err(err) => Err(err.with_path(args.path.as_bytes())),
        }
    }
    fn to_js(&self, global: &JSGlobalObject, out: Self::Out, (): ()) -> JsResult<JSValue> {
        match out {
            Some(stat) => stat::to_js(global, &stat, self.0.big_int),
            None => Ok(JSValue::UNDEFINED),
        }
    }
}

pub(crate) struct Fstat {
    fd: Fd,
    big_int: bool,
}

impl Fstat {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let fd = fd_required(global, arguments)?;
        let big_int = bigint_option(global, arguments)?;
        Ok(Fstat { fd, big_int })
    }
}

impl Operation for Fstat {
    type Out = PosixStat;
    parse_only!();
    fn run(&self) -> Maybe<PosixStat> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if statx_supported() {
            return bun_sys::fstatx(self.fd, bun_sys::STATX_MASK_FOR_STATS);
        }
        bun_sys::fstat(self.fd).map(|stat| posix_stat(&stat))
    }
    fn to_js(&self, global: &JSGlobalObject, out: PosixStat, (): ()) -> JsResult<JSValue> {
        stat::to_js(global, &out, self.big_int)
    }
}

pub(crate) struct StatFs {
    path: PathLike,
    big_int: bool,
}

impl StatFs {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let big_int = bigint_option(global, arguments)?;
        Ok(StatFs { path, big_int })
    }
}

impl Operation for StatFs {
    type Out = [u64; 8];
    parse_only!();
    fn run(&self) -> Maybe<[u64; 8]> {
        let s = bun_sys::statfs(z(&self.path)).map_err(|err| err.with_path(self.path.as_bytes()))?;
        #[cfg(target_os = "linux")]
        let frsize = s.f_frsize as u64;
        #[cfg(not(target_os = "linux"))]
        let frsize = s.f_bsize as u64;
        Ok([
            s.f_type as u64,
            s.f_bsize as u64,
            frsize,
            s.f_blocks as u64,
            s.f_bfree as u64,
            s.f_bavail as u64,
            s.f_files as u64,
            s.f_ffree as u64,
        ])
    }
    fn to_js(&self, global: &JSGlobalObject, out: [u64; 8], (): ()) -> JsResult<JSValue> {
        let object = JSObject::create(global);
        let keys = ["type", "bsize", "frsize", "blocks", "bfree", "bavail", "files", "ffree"];
        for (key, value) in keys.iter().zip(out) {
            let value = if self.big_int {
                JSValue::from_i128(i128::from(value as i64))
            } else {
                JSValue::js_number(value as f64)
            };
            object.put(global, key, value);
        }
        Ok(object.to_js())
    }
}

// ───────────────────────────── paths ─────────────────────────────

pub(crate) struct Access {
    path: PathLike,
    mode: i32,
}

impl Access {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let mode = match arguments.next_eat() {
            Some(mode) => types::mode_number_only(global, &mode)?,
            None => 0,
        };
        Ok(Access { path, mode })
    }
}

impl Operation for Access {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::access(z(&self.path), self.mode).map_err(|err| err.with_path(self.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        Ok(JSValue::NULL)
    }
}

pub(crate) struct Exists {
    path: Option<PathLike>,
}

impl Exists {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        // `existsSync` never throws: a value that is not a path is a path that does not exist.
        let path = match path_like_from_js(global, arguments) {
            Ok(path) => path,
            Err(_) => {
                global.clear_exception();
                None
            }
        };
        Ok(Exists { path })
    }
}

impl Operation for Exists {
    type Out = bool;
    parse_only!();
    fn run(&self) -> Maybe<bool> {
        Ok(self
            .path
            .as_ref()
            .is_some_and(|path| !path.is_empty() && bun_sys::access(z(path), libc::F_OK).is_ok()))
    }
    fn to_js(&self, _: &JSGlobalObject, out: bool, (): ()) -> JsResult<JSValue> {
        Ok(JSValue::js_boolean(out))
    }
}

pub(crate) struct Chmod {
    path: PathLike,
    mode: Mode,
    follow: bool,
}

impl Chmod {
    fn parse_with(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>, follow: bool) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let value = arguments.next_or_undefined();
        let mode = mode_from_js(global, &value)?
            .ok_or_else(|| invalid_arg_type(global, "mode", "of type number", &value))?;
        Ok(Chmod { path, mode, follow })
    }
}

pub(crate) struct ChmodOp<const FOLLOW: bool>(Chmod);

impl<const FOLLOW: bool> Operation for ChmodOp<FOLLOW> {
    type Out = ();
    type Keep = ();
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, ())> {
        Ok((ChmodOp(Chmod::parse_with(global, arguments, FOLLOW)?), ()))
    }
    fn run(&self) -> Maybe<()> {
        let args = &self.0;
        let result = if args.follow {
            bun_sys::chmod(z(&args.path), args.mode)
        } else {
            bun_sys::lchmod(z(&args.path), args.mode)
        };
        result.map_err(|err| err.with_path(args.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Fchmod {
    fd: Fd,
    mode: Mode,
}

impl Fchmod {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let fd = fd_required(global, arguments)?;
        let value = arguments.next_or_undefined();
        let mode = mode_from_js(global, &value)?
            .ok_or_else(|| invalid_arg_type(global, "mode", "of type number", &value))?;
        Ok(Fchmod { fd, mode })
    }
}

impl Operation for Fchmod {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::fchmod(self.fd, self.mode)
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

fn id_arg(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>, name: &str) -> JsResult<u32> {
    let Some(value) = arguments.next_eat() else {
        return Err(invalid_arguments(global, format_args!("{name} is required")));
    };
    Ok(validate_integer(global, &value, name, -1.0, f64::from(u32::MAX))? as u32)
}

pub(crate) struct ChownOp<const FOLLOW: bool> {
    path: PathLike,
    uid: u32,
    gid: u32,
}

impl<const FOLLOW: bool> ChownOp<FOLLOW> {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let uid = id_arg(global, arguments, "uid")?;
        let gid = id_arg(global, arguments, "gid")?;
        Ok(ChownOp { path, uid, gid })
    }
}

impl<const FOLLOW: bool> Operation for ChownOp<FOLLOW> {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        let result = if FOLLOW {
            bun_sys::chown(z(&self.path), self.uid, self.gid)
        } else {
            bun_sys::lchown(z(&self.path), self.uid, self.gid)
        };
        result.map_err(|err| err.with_path(self.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Fchown {
    fd: Fd,
    uid: u32,
    gid: u32,
}

impl Fchown {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let fd = fd_required(global, arguments)?;
        let uid = id_arg(global, arguments, "uid")?;
        let gid = id_arg(global, arguments, "gid")?;
        Ok(Fchown { fd, uid, gid })
    }
}

impl Operation for Fchown {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::fchown(self.fd, self.uid, self.gid)
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

fn time_arg(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>, name: &str) -> JsResult<bun_sys::TimeLike> {
    let Some(value) = arguments.next() else {
        return Err(invalid_arguments(global, format_args!("{name} is required")));
    };
    let time = types::time_like_from_js(global, &value)?
        .ok_or_else(|| invalid_arguments(global, format_args!("{name} must be a number or a Date")))?;
    arguments.eat();
    Ok(time)
}

pub(crate) struct UtimesOp<const FOLLOW: bool> {
    path: PathLike,
    atime: bun_sys::TimeLike,
    mtime: bun_sys::TimeLike,
}

impl<const FOLLOW: bool> UtimesOp<FOLLOW> {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let atime = time_arg(global, arguments, "atime")?;
        let mtime = time_arg(global, arguments, "mtime")?;
        Ok(UtimesOp { path, atime, mtime })
    }
}

impl<const FOLLOW: bool> Operation for UtimesOp<FOLLOW> {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        let result = if FOLLOW {
            bun_sys::utimens(z(&self.path), self.atime, self.mtime)
        } else {
            bun_sys::lutimens(z(&self.path), self.atime, self.mtime)
        };
        result.map_err(|err| err.with_path(self.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Futimes {
    fd: Fd,
    atime: bun_sys::TimeLike,
    mtime: bun_sys::TimeLike,
}

impl Futimes {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let fd = fd_required(global, arguments)?;
        let atime = time_arg(global, arguments, "atime")?;
        let mtime = time_arg(global, arguments, "mtime")?;
        Ok(Futimes { fd, atime, mtime })
    }
}

impl Operation for Futimes {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::futimens(self.fd, self.atime, self.mtime)
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Link {
    old_path: PathLike,
    new_path: PathLike,
}

impl Link {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let old_path = path_like_required(global, arguments, "oldPath")?;
        let new_path = path_like_required(global, arguments, "newPath")?;
        Ok(Link { old_path, new_path })
    }
}

impl Operation for Link {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::link(z(&self.old_path), z(&self.new_path))
            .map_err(|err| err.with_path_dest(self.old_path.as_bytes(), self.new_path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Symlink {
    target: PathLike,
    path: PathLike,
}

impl Symlink {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let target = path_like_required(global, arguments, "target")?;
        let path = path_like_required(global, arguments, "path")?;
        if let Some(kind) = arguments.next() {
            if !kind.is_undefined_or_null() {
                if !kind.is_string() {
                    return Err(invalid_arg_value(
                        global,
                        format_args!("Symlink type must be one of \"dir\", \"file\", or \"junction\"."),
                    ));
                }
                arguments.eat();
                let kind = kind.to_utf8(global)?;
                if !matches!(&kind[..], "dir" | "file" | "junction") {
                    return Err(invalid_arg_value(
                        global,
                        format_args!(
                            "Symlink type must be one of \"dir\", \"file\", or \"junction\". Received \"{kind}\""
                        ),
                    ));
                }
            }
        }
        Ok(Symlink { target, path })
    }
}

impl Operation for Symlink {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::symlink(z(&self.target), z(&self.path))
            .map_err(|err| err.with_path_dest(self.target.as_bytes(), self.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Readlink {
    path: PathLike,
    encoding: Encoding,
}

impl Readlink {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let encoding = encoding_arg(global, arguments, Encoding::Utf8)?;
        Ok(Readlink { path, encoding })
    }
}

impl Operation for Readlink {
    type Out = Vec<u8>;
    parse_only!();
    fn run(&self) -> Maybe<Vec<u8>> {
        let mut buf = vec![0u8; bun_core::MAX_PATH_BYTES];
        let len = bun_sys::readlink(z(&self.path), &mut buf).map_err(|err| err.with_path(self.path.as_bytes()))?;
        buf.truncate(len);
        Ok(buf)
    }
    fn to_js(&self, global: &JSGlobalObject, out: Vec<u8>, (): ()) -> JsResult<JSValue> {
        path_in(global, &out, self.encoding)
    }
}

pub(crate) struct Realpath {
    path: PathLike,
    encoding: Encoding,
}

impl Realpath {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let encoding = encoding_arg(global, arguments, Encoding::Utf8)?;
        Ok(Realpath { path, encoding })
    }
}

impl Operation for Realpath {
    type Out = Vec<u8>;
    parse_only!();
    fn run(&self) -> Maybe<Vec<u8>> {
        let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
        // SAFETY: `self.path` is NUL-terminated and `buf` holds PATH_MAX bytes and the NUL.
        let resolved = unsafe { libc::realpath(self.path.as_ptr(), buf.as_mut_ptr().cast()) };
        if resolved.is_null() {
            return Err(bun_sys::Error::from_code_int(bun_sys::last_errno(), bun_sys::Tag::realpath)
                .with_path(self.path.as_bytes()));
        }
        let len = strings::index_of_char_usize(&buf, 0).unwrap_or(buf.len());
        buf.truncate(len);
        Ok(buf)
    }
    fn to_js(&self, global: &JSGlobalObject, out: Vec<u8>, (): ()) -> JsResult<JSValue> {
        path_in(global, &out, self.encoding)
    }
}

pub(crate) struct Rename {
    old_path: PathLike,
    new_path: PathLike,
}

impl Rename {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let expected = "of type string or an instance of Buffer or URL";
        let old_path = path_like_from_js(global, arguments)?
            .ok_or_else(|| invalid_arg_type(global, "oldPath", expected, &arguments.next().unwrap_or_default()))?;
        let new_path = path_like_from_js(global, arguments)?
            .ok_or_else(|| invalid_arg_type(global, "newPath", expected, &arguments.next().unwrap_or_default()))?;
        Ok(Rename { old_path, new_path })
    }
}

impl Operation for Rename {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::rename(z(&self.old_path), z(&self.new_path))
            .map_err(|err| err.with_path_dest(self.old_path.as_bytes(), self.new_path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Unlink {
    path: PathLike,
}

impl Unlink {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        Ok(Unlink {
            path: path_like_required(global, arguments, "path")?,
        })
    }
}

impl Operation for Unlink {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::unlink(z(&self.path)).map_err(|err| err.with_path(self.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Truncate {
    path: PathOrFd,
    len: i64,
}

impl Truncate {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_or_fd_from_js(global, arguments)?
            .ok_or_else(|| invalid_arguments(global, format_args!("path must be a string or TypedArray")))?;
        let len = match arguments.next_eat() {
            Some(value) if !value.is_undefined() => {
                validate_integer(global, &value, "len", -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER)?.max(0)
            }
            _ => 0,
        };
        Ok(Truncate { path, len })
    }
}

impl Operation for Truncate {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        match &self.path {
            PathOrFd::Fd(fd) => bun_sys::ftruncate(*fd, self.len),
            PathOrFd::Path(path) => bun_sys::truncate(z(path), self.len).map_err(|err| err.with_path(path.as_bytes())),
        }
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Ftruncate {
    fd: Fd,
    len: i64,
}

impl Ftruncate {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let fd = fd_required(global, arguments)?;
        let len = match arguments.next_eat() {
            Some(value) if !value.is_undefined() => {
                validate_integer(global, &value, "len", -MAX_SAFE_INTEGER, ((1u64 << 52) - 1) as f64)?.max(0)
            }
            _ => 0,
        };
        Ok(Ftruncate { fd, len })
    }
}

impl Operation for Ftruncate {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        bun_sys::ftruncate(self.fd, self.len)
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

/// `close`, `fsync` and `fdatasync`: an fd and nothing else.
pub(crate) struct FdOp<const WHICH: u8> {
    fd: Fd,
}

pub(crate) const CLOSE: u8 = 0;
pub(crate) const FSYNC: u8 = 1;
pub(crate) const FDATASYNC: u8 = 2;

impl<const WHICH: u8> FdOp<WHICH> {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        Ok(FdOp {
            fd: fd_required(global, arguments)?,
        })
    }
}

impl<const WHICH: u8> Operation for FdOp<WHICH> {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        match WHICH {
            CLOSE => bun_sys::close(self.fd),
            FSYNC => bun_sys::fsync(self.fd),
            _ => bun_sys::fdatasync(self.fd),
        }
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

// ───────────────────────────── directories ─────────────────────────────

pub(crate) struct Mkdir {
    path: PathLike,
    recursive: bool,
    mode: Mode,
}

impl Mkdir {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let mut recursive = false;
        let mut mode: Mode = 0o777;
        if let Some(value) = arguments.next_eat() {
            if value.is_object() {
                recursive = boolean_strict(global, &value, "recursive")?.unwrap_or(false);
                if let Some(m) = value.get(global, "mode")? {
                    mode = mode_from_js(global, &m)?.unwrap_or(mode);
                }
            }
            if value.is_number() || value.is_string() {
                mode = mode_from_js(global, &value)?.unwrap_or(mode);
            }
        }
        Ok(Mkdir { path, recursive, mode })
    }
}

fn is_directory(path: &ZStr) -> bool {
    bun_sys::stat(path).is_ok_and(|stat| bun_sys::kind_from_mode(stat.st_mode as Mode) == bun_sys::FileKind::Directory)
}

/// `NodeFS::mkdir_recursive_os_path_impl`: the first directory it made, `None` where all of them were there.
pub(crate) fn mkdir_recursive(path: &[u8], mode: Mode) -> Maybe<Option<Vec<u8>>> {
    let zpath = ZBox::from_bytes(path);
    match bun_sys::mkdir(&zpath, mode) {
        Ok(()) => return Ok(Some(path.to_vec())),
        Err(err) => match err.get_errno() {
            E::EISDIR | E::EEXIST => {
                if is_directory(&zpath) {
                    return Ok(None);
                }
                return Err(bun_sys::Error::from_code_int(i32::from(err.errno), bun_sys::Tag::mkdir).with_path(path));
            }
            E::ENOENT if !path.is_empty() => {}
            _ => return Err(err.with_path(path)),
        },
    }
    let is_sep = |at: &usize| path[*at] == b'/';
    let after_root = usize::from(path.first() == Some(&b'/')).max(1);
    let mut first_match = 0;
    for end in (after_root..path.len()).rev().filter(is_sep) {
        match bun_sys::mkdir(&ZBox::from_bytes(&path[..end]), mode) {
            Ok(()) => {}
            Err(err) => match err.get_errno() {
                E::ENOENT => continue,
                E::EEXIST => {}
                _ => return Err(err.with_path(&path[..end])),
            },
        }
        first_match = end;
        break;
    }
    for end in (after_root.max(first_match + 1)..path.len()).filter(is_sep) {
        match bun_sys::mkdir(&ZBox::from_bytes(&path[..end]), mode) {
            Ok(()) => {}
            Err(err) if err.get_errno() == E::EEXIST => {}
            Err(err) => return Err(err.with_path(path)),
        }
    }
    if let Err(err) = bun_sys::mkdir(&zpath, mode)
        && err.get_errno() != E::EEXIST
    {
        return Err(err.with_path(path));
    }
    Ok(Some(path[..first_match].to_vec()))
}

impl Operation for Mkdir {
    type Out = Option<Vec<u8>>;
    parse_only!();
    fn run(&self) -> Maybe<Self::Out> {
        if self.path.is_empty() {
            return Err(bun_sys::Error::from_code(E::ENOENT, bun_sys::Tag::mkdir).with_path(b""));
        }
        if self.recursive {
            return mkdir_recursive(self.path.as_bytes(), self.mode);
        }
        bun_sys::mkdir(z(&self.path), self.mode)
            .map(|()| None)
            .map_err(|err| err.with_path(self.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, out: Self::Out, (): ()) -> JsResult<JSValue> {
        Ok(match out {
            Some(first) => JSValue::from_utf8(&first),
            None => JSValue::UNDEFINED,
        })
    }
}

pub(crate) struct Mkdtemp {
    prefix: PathLike,
    encoding: Encoding,
}

impl Mkdtemp {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let prefix = path_like_from_js(global, arguments)?.ok_or_else(|| {
            invalid_arg_type(
                global,
                "prefix",
                "of type string or an instance of Buffer or URL",
                &arguments.next().unwrap_or_default(),
            )
        })?;
        let encoding = encoding_arg(global, arguments, Encoding::Utf8)?;
        Ok(Mkdtemp { prefix, encoding })
    }
}

impl Operation for Mkdtemp {
    type Out = Vec<u8>;
    parse_only!();
    fn run(&self) -> Maybe<Vec<u8>> {
        let mut template = [self.prefix.as_bytes(), b"XXXXXX\0"].concat();
        match bun_sys::mkdtemp(&mut template) {
            Ok(name) => Ok(name.to_vec()),
            Err(err) => Err(err.with_path(&template[..template.len() - 1])),
        }
    }
    fn to_js(&self, global: &JSGlobalObject, out: Vec<u8>, (): ()) -> JsResult<JSValue> {
        path_in(global, &out, self.encoding)
    }
}

pub(crate) struct Rmdir {
    path: PathLike,
    recursive: bool,
    force: bool,
    max_retries: u32,
    retry_delay: u32,
}

impl Rmdir {
    /// `RmDir::from_js_impl`; `fs.rm` reads only own properties and refuses a boolean that is not one.
    fn parse_with(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>, strict: bool) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let mut options = Rmdir {
            path,
            recursive: false,
            force: false,
            max_retries: 0,
            retry_delay: 100,
        };
        let Some(value) = arguments.next_eat() else {
            return Ok(options);
        };
        if value.is_object() {
            let get = |key: &str| -> JsResult<Option<JSValue>> {
                if strict && !own_property(global, &value, key)? {
                    return Ok(None);
                }
                value.get(global, key)
            };
            if let Some(boolean) = get("recursive")? {
                if !boolean.is_boolean() {
                    return Err(invalid_arguments(
                        global,
                        format_args!("The \"options.recursive\" property must be of type boolean."),
                    ));
                }
                options.recursive = boolean.as_boolean();
            }
            if let Some(boolean) = get("force")? {
                if !boolean.is_boolean() {
                    return Err(invalid_arguments(
                        global,
                        format_args!("The \"options.force\" property must be of type boolean."),
                    ));
                }
                options.force = boolean.as_boolean();
            }
            if let Some(delay) = get("retryDelay")? {
                options.retry_delay =
                    validate_integer(global, &delay, "options.retryDelay", 0.0, f64::from(u32::MAX))? as u32;
            }
            if let Some(retries) = get("maxRetries")? {
                options.max_retries =
                    validate_integer(global, &retries, "options.maxRetries", 0.0, f64::from(u32::MAX))? as u32;
            }
        } else if !value.is_undefined() {
            return Err(invalid_arguments(
                global,
                format_args!("The \"options\" argument must be of type object."),
            ));
        }
        Ok(options)
    }
}

fn own_property(global: &JSGlobalObject, object: &JSValue, key: &str) -> JsResult<bool> {
    let has_own = global
        .evaluate_script("Object.prototype.hasOwnProperty")?
        .call(global, object.clone(), &[JSValue::from_utf8(key.as_bytes())])?;
    Ok(has_own.to_boolean())
}

/// Removes `path` and what is in it, without following a link.
fn remove_tree(path: &[u8]) -> Maybe<()> {
    let zpath = ZBox::from_bytes(path);
    let stat = bun_sys::lstat(&zpath).map_err(|err| err.with_path(path))?;
    if bun_sys::kind_from_mode(stat.st_mode as Mode) != bun_sys::FileKind::Directory {
        return bun_sys::unlink(&zpath).map_err(|err| err.with_path(path));
    }
    let names = list_directory(&zpath).map_err(|err| err.with_path(path))?;
    for (name, _) in names {
        let mut child = path.to_vec();
        if !child.ends_with(b"/") {
            child.push(b'/');
        }
        child.extend_from_slice(&name);
        remove_tree(&child)?;
    }
    bun_sys::rmdir(&zpath).map_err(|err| err.with_path(path))
}

fn retry<T>(max_retries: u32, delay_ms: u32, mut f: impl FnMut() -> Maybe<T>) -> Maybe<T> {
    let mut attempt = 0;
    loop {
        match f() {
            Err(err)
                if attempt < max_retries
                    && matches!(err.get_errno(), E::EBUSY | E::EMFILE | E::ENFILE | E::ENOTEMPTY | E::EPERM) =>
            {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(u64::from(delay_ms) * u64::from(attempt)));
            }
            result => return result,
        }
    }
}

pub(crate) struct RmdirOp(Rmdir);

impl Operation for RmdirOp {
    type Out = ();
    type Keep = ();
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, ())> {
        Ok((RmdirOp(Rmdir::parse_with(global, arguments, false)?), ()))
    }
    fn run(&self) -> Maybe<()> {
        let args = &self.0;
        if args.recursive {
            return retry(args.max_retries, args.retry_delay, || remove_tree(args.path.as_bytes()));
        }
        bun_sys::rmdir(z(&args.path)).map_err(|err| err.with_path(args.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

pub(crate) struct Rm(pub(crate) Rmdir);

impl Rm {
    pub(crate) fn options(&self) -> (u32, u32, bool, bool) {
        (self.0.retry_delay, self.0.max_retries, self.0.recursive, self.0.force)
    }
}

impl Operation for Rm {
    type Out = ();
    type Keep = ();
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, ())> {
        Ok((Rm(Rmdir::parse_with(global, arguments, true)?), ()))
    }
    fn run(&self) -> Maybe<()> {
        let args = &self.0;
        let path = args.path.as_bytes();
        let result = retry(args.max_retries, args.retry_delay, || {
            if args.recursive {
                return remove_tree(path);
            }
            let stat = bun_sys::lstat(z(&args.path)).map_err(|err| err.with_path(path))?;
            if bun_sys::kind_from_mode(stat.st_mode as Mode) == bun_sys::FileKind::Directory {
                return Err(bun_sys::Error::from_code(E::EISDIR, bun_sys::Tag::rm).with_path(path));
            }
            bun_sys::unlink(z(&args.path)).map_err(|err| err.with_path(path))
        });
        match result {
            Err(err) if args.force && err.get_errno() == E::ENOENT => Ok(()),
            result => result,
        }
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

/// The names in a directory and their kinds, `.` and `..` left out. A kind the listing does not give is looked up.
fn list_directory(path: &ZStr) -> Maybe<Vec<(Vec<u8>, bun_sys::FileKind)>> {
    let dir = bun_sys::open(path, O::DIRECTORY | O::RDONLY | O::CLOEXEC, 0)?;
    let dir = bun_sys::File::from_fd(dir);
    let mut iterator = bun_sys::dir_iterator::iterate(dir.fd());
    let mut entries = Vec::new();
    while let Some(entry) = iterator.next()? {
        let name = entry.name.slice_u8();
        let mut kind = entry.kind;
        if kind == bun_sys::FileKind::Unknown {
            if let Ok(stat) = bun_sys::lstatat(dir.fd(), entry.name.as_zstr()) {
                kind = bun_sys::kind_from_mode(stat.st_mode as Mode);
            }
        }
        entries.push((name.to_vec(), kind));
    }
    Ok(entries)
}

pub(crate) struct Readdir {
    path: PathLike,
    encoding: Encoding,
    with_file_types: bool,
    recursive: bool,
}

impl Readdir {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let mut options = Readdir {
            path,
            encoding: Encoding::Utf8,
            with_file_types: false,
            recursive: false,
        };
        if let Some(value) = arguments.next_eat() {
            if value.is_string() || value.is_string_object() {
                options.encoding = Encoding::assert(global, &value.to_js_string(global)?, Encoding::Utf8)?;
            } else if value.is_object() {
                if let Some(encoding) = value.get(global, "encoding")? {
                    options.encoding = Encoding::assert(global, &encoding, Encoding::Utf8)?;
                }
                options.recursive = boolean_strict(global, &value, "recursive")?.unwrap_or(false);
                options.with_file_types = boolean_strict(global, &value, "withFileTypes")?.unwrap_or(false);
            }
        }
        Ok(options)
    }
}

/// An entry of `readdir`: its name (relative to the root when recursive), its kind, its parent's path.
pub(crate) struct ReaddirEntry {
    name: Vec<u8>,
    kind: bun_sys::FileKind,
    parent: Vec<u8>,
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    if dir.is_empty() {
        return name.to_vec();
    }
    let mut out = dir.to_vec();
    if !out.ends_with(b"/") {
        out.push(b'/');
    }
    out.extend_from_slice(name);
    out
}

fn skips_subdirectory(errno: E) -> bool {
    matches!(errno, E::ENOENT | E::ENOTDIR | E::EACCES | E::EPERM | E::ELOOP)
}

impl Operation for Readdir {
    type Out = Vec<ReaddirEntry>;
    parse_only!();
    fn run(&self) -> Maybe<Self::Out> {
        let root = self.path.as_bytes();
        let scandir = |err: bun_sys::Error| {
            bun_sys::Error::from_code_int(i32::from(err.errno), bun_sys::Tag::scandir).with_path(root)
        };
        if !self.recursive {
            return Ok(list_directory(z(&self.path))
                .map_err(scandir)?
                .into_iter()
                .map(|(name, kind)| ReaddirEntry {
                    name,
                    kind,
                    parent: root.to_vec(),
                })
                .collect());
        }
        let mut entries = Vec::new();
        let mut pending: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::from([Vec::new()]);
        let mut is_root = true;
        while let Some(relative) = pending.pop_front() {
            let dir = join(root, &relative);
            let listed = match list_directory(&ZBox::from_bytes(&dir)) {
                Ok(listed) => listed,
                Err(err) if !is_root && skips_subdirectory(err.get_errno()) => continue,
                Err(err) => return Err(scandir(err)),
            };
            is_root = false;
            for (name, kind) in listed {
                let relative_name = join(&relative, &name);
                if matches!(kind, bun_sys::FileKind::Directory | bun_sys::FileKind::SymLink) {
                    pending.push_back(relative_name.clone());
                }
                entries.push(ReaddirEntry {
                    name: if self.with_file_types { name } else { relative_name },
                    kind,
                    parent: dir.clone(),
                });
            }
        }
        Ok(entries)
    }
    fn to_js(&self, global: &JSGlobalObject, out: Self::Out, (): ()) -> JsResult<JSValue> {
        let array = JSArray::create(global, &[]);
        let mut parent_value: Option<(Vec<u8>, JSValue)> = None;
        for entry in out {
            let name = self.encoding.to_js(global, &entry.name)?;
            let value = if self.with_file_types {
                let parent = match &parent_value {
                    Some((bytes, value)) if *bytes == entry.parent => value.clone(),
                    _ => {
                        let value = self.encoding.to_js(global, &entry.parent)?;
                        parent_value = Some((entry.parent.clone(), value.clone()));
                        value
                    }
                };
                stat::dirent(global, name, stat::dirent_type(entry.kind), parent)?
            } else {
                name
            };
            array.push(global, value)?;
        }
        Ok(array.to_js())
    }
}

// ───────────────────────────── file descriptors ─────────────────────────────

pub(crate) struct Open {
    path: PathLike,
    flags: i32,
    mode: Mode,
}

impl Open {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let path = path_like_required(global, arguments, "path")?;
        let mut flags = O::RDONLY;
        let mut mode = DEFAULT_PERMISSION;
        if let Some(value) = arguments.next_eat() {
            if !value.is_undefined_or_null() {
                flags = types::flags_from_js(global, &value)?.unwrap_or(flags);
            }
            if let Some(next) = arguments.next_eat() {
                mode = mode_from_js(global, &next)?.unwrap_or(mode);
            }
        }
        Ok(Open { path, flags, mode })
    }
}

impl Operation for Open {
    type Out = i32;
    parse_only!();
    fn run(&self) -> Maybe<i32> {
        bun_sys::open(z(&self.path), self.flags | O::CLOEXEC, self.mode)
            .map(|fd| fd.native())
            .map_err(|err| err.with_path(self.path.as_bytes()))
    }
    fn to_js(&self, _: &JSGlobalObject, out: i32, (): ()) -> JsResult<JSValue> {
        Ok(JSValue::js_number_from_int32(out))
    }
}

/// `i52::offset_from_js`: a position that is a number or a BigInt, `None` for anything else.
fn offset_from_js(global: &JSGlobalObject, value: &JSValue) -> JsResult<Option<i64>> {
    if value.is_number() {
        let n = value.as_number();
        return Ok(n.is_finite().then_some(n as i64));
    }
    if value.is_big_int() {
        return Ok(Some(<i64 as bun_js::CoerceTo>::coerce_from(value.clone(), global)?));
    }
    Ok(None)
}

fn position_of(position: Option<i64>) -> Option<i64> {
    position.filter(|&p| p >= 0)
}

pub(crate) struct Read {
    fd: Fd,
    length: usize,
    position: Option<i64>,
}

/// The buffer `read` fills, and where in it.
pub(crate) struct ReadInto {
    buffer: JSValue,
    offset: usize,
}

const BUFFER_TYPES: &str = "an instance of Buffer, TypedArray, or DataView";

impl Operation for Read {
    type Out = Vec<u8>;
    type Keep = ReadInto;
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, ReadInto)> {
        let fd = fd_required(global, arguments)?;
        let buffer = arguments
            .next_eat()
            .ok_or_else(|| invalid_arguments(global, format_args!("buffer is required")))?;
        let Some(buf_len) = buffer.array_buffer_view_byte_length(global)? else {
            return Err(invalid_arg_type(global, "buffer", BUFFER_TYPES, &buffer));
        };
        let offset_value = arguments.next_eat().unwrap_or(JSValue::NULL);
        let offset = if offset_value.is_undefined_or_null() {
            0
        } else {
            validate_integer(global, &offset_value, "offset", 0.0, MAX_SAFE_INTEGER)? as u64
        };
        let length_float = match arguments.next_eat() {
            Some(value) => value.to_number(global)?,
            None => 0.0,
        };
        let keep = |offset: u64| ReadInto {
            buffer: buffer.clone(),
            offset: offset as usize,
        };
        if length_float == 0.0 {
            return Ok((Read { fd, length: 0, position: None }, keep(0)));
        }
        let length_value = JSValue::js_number(length_float);
        if buf_len == 0 {
            return Err(invalid_arg_value(
                global,
                format_args!("The argument 'buffer' is empty and cannot be written."),
            ));
        }
        if length_float.fract() != 0.0 {
            return Err(out_of_range(global, "length", "an integer", &length_value));
        }
        let length = length_float as i64;
        if length > 0 && length as usize > buf_len {
            return Err(out_of_range(global, "length", &format!("<= {buf_len}"), &length_value));
        }
        if (offset as i64).saturating_add(length) > buf_len as i64 {
            let max = (buf_len as u64).saturating_sub(offset);
            return Err(out_of_range(global, "length", &format!("<= {max}"), &length_value));
        }
        if length < 0 {
            return Err(out_of_range(global, "length", ">= 0", &length_value));
        }
        let position_value = arguments.next_eat().unwrap_or(JSValue::NULL);
        let position = if position_value.is_undefined_or_null() {
            -1
        } else if position_value.is_number() {
            validate_integer(global, &position_value, "position", -1.0, MAX_SAFE_INTEGER)?
        } else if position_value.is_big_int() {
            let position = <i64 as bun_js::CoerceTo>::coerce_from(position_value.clone(), global)?;
            let max = i64::MAX - length;
            if position < -1 || position > max {
                return Err(out_of_range(global, "position", &format!(">= -1 && <= {max}"), &position_value));
            }
            position
        } else {
            return Err(invalid_arg_type(global, "position", "of type number or bigint", &position_value));
        };
        Ok((
            Read {
                fd,
                length: length as usize,
                position: position_of(Some(position)),
            },
            keep(offset),
        ))
    }
    fn run(&self) -> Maybe<Vec<u8>> {
        let mut buf = vec![0u8; self.length];
        if self.length == 0 {
            return Ok(buf);
        }
        let read = match self.position {
            Some(position) => bun_sys::pread(self.fd, &mut buf, position),
            None => bun_sys::read(self.fd, &mut buf),
        }?;
        buf.truncate(read);
        Ok(buf)
    }
    fn to_js(&self, global: &JSGlobalObject, out: Vec<u8>, keep: ReadInto) -> JsResult<JSValue> {
        keep.buffer.write_array_buffer_view(global, keep.offset, &out)?;
        Ok(JSValue::js_number(out.len() as f64))
    }
}

/// The buffers of `readv` and `writev`: an array of views.
fn views_of(global: &JSGlobalObject, value: &JSValue) -> JsResult<Vec<JSValue>> {
    if !value.is_array() {
        return Err(invalid_arg_type(global, "buffers", "an instance of ArrayBufferView[]", value));
    }
    let length = value.get_length(global)?;
    let mut views = Vec::with_capacity(length as usize);
    for i in 0..length as u32 {
        let view = value.get_index(global, i)?;
        if !view.is_array_buffer_view() {
            return Err(invalid_arg_type(global, "buffers", "an instance of ArrayBufferView[]", value));
        }
        views.push(view);
    }
    Ok(views)
}

fn vector_position(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Option<i64>> {
    Ok(match arguments.next_eat() {
        Some(value) => position_of(offset_from_js(global, &value)?),
        None => None,
    })
}

pub(crate) struct Readv {
    fd: Fd,
    lengths: Vec<usize>,
    position: Option<i64>,
}

impl Operation for Readv {
    type Out = Vec<u8>;
    type Keep = Vec<JSValue>;
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, Vec<JSValue>)> {
        let fd = fd_required(global, arguments)?;
        let buffers = arguments
            .next_eat()
            .ok_or_else(|| invalid_arguments(global, format_args!("Expected an ArrayBufferView[]")))?;
        let views = views_of(global, &buffers)?;
        let mut lengths = Vec::with_capacity(views.len());
        for view in &views {
            lengths.push(view.array_buffer_view_byte_length(global)?.unwrap_or(0));
        }
        let position = vector_position(global, arguments)?;
        Ok((Readv { fd, lengths, position }, views))
    }
    fn run(&self) -> Maybe<Vec<u8>> {
        let mut buf = vec![0u8; self.lengths.iter().sum()];
        if buf.is_empty() {
            return Ok(buf);
        }
        let read = match self.position {
            Some(position) => bun_sys::pread(self.fd, &mut buf, position),
            None => bun_sys::read(self.fd, &mut buf),
        }?;
        buf.truncate(read);
        Ok(buf)
    }
    fn to_js(&self, global: &JSGlobalObject, out: Vec<u8>, views: Vec<JSValue>) -> JsResult<JSValue> {
        let mut rest = &out[..];
        for view in views {
            if rest.is_empty() {
                break;
            }
            let n = view.write_array_buffer_view(global, 0, rest)?;
            rest = &rest[n..];
        }
        Ok(JSValue::js_number(out.len() as f64))
    }
}

/// `write(fd, all, position)` until all of it is written.
fn write_all(fd: Fd, mut data: &[u8], mut position: Option<i64>) -> Maybe<usize> {
    let total = data.len();
    while !data.is_empty() {
        let written = match position {
            Some(at) => bun_sys::pwrite(fd, data, at),
            None => bun_sys::write(fd, data),
        }?;
        if written == 0 {
            break;
        }
        data = &data[written..];
        position = position.map(|at| at + written as i64);
    }
    Ok(total - data.len())
}

pub(crate) struct Writev {
    fd: Fd,
    data: Vec<u8>,
    position: Option<i64>,
}

impl Writev {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let fd = fd_required(global, arguments)?;
        let buffers = arguments
            .next_eat()
            .ok_or_else(|| invalid_arguments(global, format_args!("Expected an ArrayBufferView[]")))?;
        let mut data = Vec::new();
        for view in views_of(global, &buffers)? {
            data.extend(view.array_buffer_view_bytes(global)?.unwrap_or_default());
        }
        let position = vector_position(global, arguments)?;
        Ok(Writev { fd, data, position })
    }
}

impl Operation for Writev {
    type Out = usize;
    parse_only!();
    fn run(&self) -> Maybe<usize> {
        if self.data.is_empty() {
            return Ok(0);
        }
        match self.position {
            Some(position) => bun_sys::pwrite(self.fd, &self.data, position),
            None => bun_sys::write(self.fd, &self.data),
        }
    }
    fn to_js(&self, _: &JSGlobalObject, out: usize, (): ()) -> JsResult<JSValue> {
        Ok(JSValue::js_number(out as f64))
    }
}

pub(crate) struct Write {
    fd: Fd,
    data: Vec<u8>,
    position: Option<i64>,
}

impl Write {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let fd = fd_required(global, arguments)?;
        let value = arguments
            .next_eat()
            .ok_or_else(|| invalid_arguments(global, format_args!("data is required")))?;
        let is_view = value.is_array_buffer_view() || value.is_uint8_array_data_view_or_array_buffer();
        if !is_view && !value.is_string() {
            return Err(invalid_arg_type(global, "buffer", "of type string or an instance of TypedArray", &value));
        }
        let mut position = None;
        if is_view {
            let mut bytes = types::bytes_of_view(global, &value)?.unwrap_or_default();
            let mut offset = 0usize;
            let mut length = bytes.len();
            'parse: {
                let Some(current) = arguments.next() else { break 'parse };
                if current.is_undefined_or_null() || current.is_callable() {
                    break 'parse;
                }
                offset = validate_integer(global, &current, "offset", 0.0, MAX_SAFE_INTEGER)? as usize;
                arguments.eat();
                if offset > bytes.len() {
                    return Err(out_of_range(global, "offset", &format!("<= {}", bytes.len()), &current));
                }
                length = bytes.len() - offset;
                let Some(current) = arguments.next() else { break 'parse };
                if !(current.is_number() || current.is_big_int()) {
                    break 'parse;
                }
                let wanted = <i64 as bun_js::CoerceTo>::coerce_from(current.clone(), global)?;
                let max = ((bytes.len() - offset) as i64).min(i64::from(i32::MAX));
                if wanted > max || wanted < 0 {
                    return Err(out_of_range(global, "length", &format!(">= 0 && <= {max}"), &current));
                }
                length = wanted as usize;
                arguments.eat();
                let Some(current) = arguments.next() else { break 'parse };
                if !(current.is_number() || current.is_big_int()) {
                    break 'parse;
                }
                position = position_of(offset_from_js(global, &current)?);
                arguments.eat();
            }
            bytes.truncate(offset + length);
            bytes.drain(..offset);
            return Ok(Write { fd, data: bytes, position });
        }
        let mut encoding = Encoding::Utf8;
        if let Some(current) = arguments.next() {
            position = position_of(offset_from_js(global, &current)?);
            arguments.eat();
            if let Some(next) = arguments.next() {
                if next.is_string() {
                    encoding = Encoding::assert(global, &next, encoding)?;
                    arguments.eat();
                }
            }
        }
        let data = encoding.decode(global, &value)?;
        Ok(Write { fd, data, position })
    }
}

impl Operation for Write {
    type Out = usize;
    parse_only!();
    fn run(&self) -> Maybe<usize> {
        if self.data.is_empty() {
            return Ok(0);
        }
        match self.position {
            Some(position) => bun_sys::pwrite(self.fd, &self.data, position),
            None => bun_sys::write(self.fd, &self.data),
        }
    }
    fn to_js(&self, _: &JSGlobalObject, out: usize, (): ()) -> JsResult<JSValue> {
        Ok(JSValue::js_number(out as f64))
    }
}

// ───────────────────────────── whole files ─────────────────────────────

fn signal_aborted(global: &JSGlobalObject, options: &JSValue) -> JsResult<Option<JSValue>> {
    let Some(signal) = options.get(global, "signal")?.filter(JSValue::is_truthy) else {
        return Ok(None);
    };
    let aborted = signal.get(global, "aborted")?;
    if aborted.is_none() && signal.get(global, "addEventListener")?.is_none() {
        return Err(invalid_arg_type(global, "signal", "an instance of AbortSignal", &signal));
    }
    Ok(aborted.filter(JSValue::is_truthy).map(|_| signal))
}

/// `$makeAbortError` as node:fs's: an `AbortError` whose cause is the signal's reason.
fn abort_error(global: &JSGlobalObject, signal: &JSValue) -> JsResult<JSValue> {
    let reason = signal.get(global, "reason")?.unwrap_or_default();
    let error = global.create_error_instance(format_args!("The operation was aborted"));
    error.put(global, "name", JSValue::from_utf8(b"AbortError"));
    error.put(global, "code", JSValue::from_utf8(b"ABORT_ERR"));
    if !reason.is_undefined() {
        error.put(global, "cause", reason);
    }
    Ok(error)
}

pub(crate) struct ReadFile {
    path: PathOrFd,
    encoding: Encoding,
    flag: i32,
}

/// `ReadFile`'s arguments, and the error of a signal that has aborted already.
impl ReadFile {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, Option<JSValue>)> {
        let path = path_or_fd_from_js(global, arguments)?
            .ok_or_else(|| invalid_arguments(global, format_args!("path must be a string or a file descriptor")))?;
        let mut encoding = Encoding::Buffer;
        let mut flag = O::RDONLY;
        let mut aborted = None;
        if let Some(value) = arguments.next_eat() {
            if value.is_string() {
                encoding = Encoding::assert(global, &value, encoding)?;
            } else if value.is_object() {
                if let Some(e) = value.get(global, "encoding")? {
                    encoding = Encoding::assert(global, &e, encoding)?;
                }
                if let Some(f) = value.get(global, "flag")?.filter(JSValue::is_truthy) {
                    flag = types::flags_from_js(global, &f)?.unwrap_or(flag);
                }
                if let Some(signal) = signal_aborted(global, &value)? {
                    aborted = Some(abort_error(global, &signal)?);
                }
            }
        }
        Ok((ReadFile { path, encoding, flag }, aborted))
    }
}

fn read_to_end(fd: Fd, size_hint: usize) -> Maybe<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(size_hint.max(64) + 1);
    loop {
        if out.len() == out.capacity() {
            out.reserve(out.capacity().max(8192));
        }
        let start = out.len();
        out.resize(out.capacity(), 0);
        match bun_sys::read(fd, &mut out[start..]) {
            Ok(0) => {
                out.truncate(start);
                return Ok(out);
            }
            Ok(n) => out.truncate(start + n),
            Err(err) if err.get_errno() == E::EINTR => out.truncate(start),
            Err(err) => {
                out.truncate(start);
                return Err(err);
            }
        }
    }
}

impl Operation for ReadFile {
    type Out = Vec<u8>;
    type Keep = ();
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, ())> {
        let (op, aborted) = Self::parse(global, arguments)?;
        if let Some(error) = aborted {
            return Err(global.throw_value(error));
        }
        Ok((op, ()))
    }
    fn run(&self) -> Maybe<Vec<u8>> {
        match &self.path {
            PathOrFd::Fd(fd) => read_to_end(*fd, 0),
            PathOrFd::Path(path) => {
                let fd = bun_sys::open(z(path), self.flag | O::CLOEXEC | O::NOCTTY, 0)
                    .map_err(|err| err.with_path(path.as_bytes()))?;
                let file = bun_sys::File::from_fd(fd);
                let size = bun_sys::fstat(file.fd()).map_or(0, |stat| stat.st_size.max(0) as usize);
                read_to_end(file.fd(), size).map_err(|err| err.with_path(path.as_bytes()))
            }
        }
    }
    fn to_js(&self, global: &JSGlobalObject, out: Vec<u8>, (): ()) -> JsResult<JSValue> {
        self.encoding.to_js(global, &out)
    }
}

pub(crate) struct WriteFile {
    file: PathOrFd,
    data: Vec<u8>,
    flag: i32,
    mode: Mode,
    flush: bool,
}

impl WriteFile {
    fn parse_with(
        global: &JSGlobalObject,
        arguments: &mut ArgumentsSlice<'_>,
        default_flag: i32,
    ) -> JsResult<(Self, Option<JSValue>)> {
        let file = path_or_fd_from_js(global, arguments)?
            .ok_or_else(|| invalid_arguments(global, format_args!("path must be a string or a file descriptor")))?;
        let data_value = arguments
            .next_eat()
            .ok_or_else(|| invalid_arguments(global, format_args!("data is required")))?;
        let mut encoding = if data_value.is_string() {
            Encoding::Utf8
        } else {
            Encoding::Buffer
        };
        let mut flag = default_flag;
        let mut mode = DEFAULT_PERMISSION;
        let mut flush = false;
        let mut aborted = None;
        if let Some(value) = arguments.next_eat() {
            if value.is_string() {
                encoding = Encoding::assert(global, &value, encoding)?;
            } else if value.is_object() {
                if let Some(e) = value.get(global, "encoding")? {
                    encoding = Encoding::assert(global, &e, encoding)?;
                }
                if let Some(f) = value.get(global, "flag")?.filter(JSValue::is_truthy) {
                    flag = types::flags_from_js(global, &f)?.unwrap_or(flag);
                }
                if let Some(m) = value.get(global, "mode")?.filter(JSValue::is_truthy) {
                    mode = mode_from_js(global, &m)?.unwrap_or(mode);
                }
                if let Some(signal) = signal_aborted(global, &value)? {
                    aborted = Some(abort_error(global, &signal)?);
                }
                if let Some(f) = value.get(global, "flush")? {
                    if !(f.is_boolean() || f.is_undefined_or_null()) {
                        return Err(invalid_arg_type(global, "flush", "of type boolean", &f));
                    }
                    flush = f.is_boolean() && f.as_boolean();
                }
            }
        }
        let data = if data_value.is_string() {
            encoding.decode(global, &data_value)?
        } else {
            types::bytes_of_view(global, &data_value)?.ok_or_else(|| {
                invalid_arg_type(
                    global,
                    "data",
                    "of type string or an instance of Buffer, TypedArray, or DataView",
                    &data_value,
                )
            })?
        };
        Ok((
            WriteFile {
                file,
                data,
                flag,
                mode,
                flush,
            },
            aborted,
        ))
    }

    fn write(&self) -> Maybe<()> {
        match &self.file {
            PathOrFd::Fd(fd) => {
                write_all(*fd, &self.data, None)?;
                if self.flush {
                    bun_sys::fsync(*fd)?;
                }
                Ok(())
            }
            PathOrFd::Path(path) => {
                let fd = bun_sys::open(z(path), self.flag | O::CLOEXEC, self.mode)
                    .map_err(|err| err.with_path(path.as_bytes()))?;
                let file = bun_sys::File::from_fd(fd);
                write_all(file.fd(), &self.data, None).map_err(|err| err.with_path(path.as_bytes()))?;
                if self.flush {
                    bun_sys::fsync(file.fd())?;
                }
                Ok(())
            }
        }
    }
}

/// `writeFile` and `appendFile`: one but for the flag they open a path with.
pub(crate) struct WriteFileOp<const APPEND: bool>(WriteFile);

impl<const APPEND: bool> Operation for WriteFileOp<APPEND> {
    type Out = ();
    type Keep = ();
    fn from_js(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<(Self, ())> {
        let flag = if APPEND {
            O::APPEND | O::CREAT | O::WRONLY
        } else {
            O::TRUNC | O::CREAT | O::WRONLY
        };
        let (op, aborted) = WriteFile::parse_with(global, arguments, flag)?;
        if let Some(error) = aborted {
            return Err(global.throw_value(error));
        }
        Ok((WriteFileOp(op), ()))
    }
    fn run(&self) -> Maybe<()> {
        self.0.write()
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

/// Copies `src` onto `dest`, created with `src`'s mode, or refused where it exists and `exclusive`.
fn copy_regular_file(src: &[u8], dest: &[u8], exclusive: bool) -> Maybe<()> {
    let with_paths = |err: bun_sys::Error| {
        bun_sys::Error::from_code_int(i32::from(err.errno), bun_sys::Tag::copyfile).with_path_dest(src, dest)
    };
    let input = bun_sys::open(&ZBox::from_bytes(src), O::RDONLY | O::CLOEXEC, 0).map_err(with_paths)?;
    let input = bun_sys::File::from_fd(input);
    let stat = bun_sys::fstat(input.fd()).map_err(with_paths)?;
    if bun_sys::kind_from_mode(stat.st_mode as Mode) == bun_sys::FileKind::Directory {
        return Err(bun_sys::Error::from_code(E::EISDIR, bun_sys::Tag::copyfile).with_path_dest(src, dest));
    }
    let flags = O::WRONLY | O::CREAT | O::TRUNC | O::CLOEXEC | if exclusive { O::EXCL } else { 0 };
    let output = bun_sys::open(&ZBox::from_bytes(dest), flags, stat.st_mode as Mode).map_err(with_paths)?;
    let output = bun_sys::File::from_fd(output);
    bun_sys::copy_file(input.fd(), output.fd()).map_err(with_paths)?;
    bun_sys::fchmod(output.fd(), stat.st_mode as Mode & 0o7777).map_err(with_paths)
}

pub(crate) struct CopyFile {
    src: PathLike,
    dest: PathLike,
    mode: i32,
}

impl CopyFile {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let src = path_like_required(global, arguments, "src")?;
        let dest = path_like_required(global, arguments, "dest")?;
        let mode = match arguments.next_eat() {
            Some(mode) => types::mode_number_only(global, &mode)?,
            None => 0,
        };
        Ok(CopyFile { src, dest, mode })
    }
}

const COPYFILE_EXCL: i32 = 1;
const COPYFILE_FICLONE_FORCE: i32 = 4;

impl Operation for CopyFile {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        if self.mode & COPYFILE_FICLONE_FORCE != 0 {
            return Err(bun_sys::Error::from_code(E::ENOTSUP, bun_sys::Tag::copyfile)
                .with_path_dest(self.src.as_bytes(), self.dest.as_bytes()));
        }
        copy_regular_file(self.src.as_bytes(), self.dest.as_bytes(), self.mode & COPYFILE_EXCL != 0)
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}

/// `fs.cp`'s native fast path (`NodeFS::cp`): what src/js/internal/fs/cp-sync.ts hands it when it has no filter,
/// no `dereference`, no timestamps to keep and no `mode`.
pub(crate) struct Cp {
    src: PathLike,
    dest: PathLike,
    recursive: bool,
    error_on_exist: bool,
    force: bool,
}

impl Cp {
    fn parse(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Self> {
        let src = path_like_required(global, arguments, "src")?;
        let dest = path_like_required(global, arguments, "dest")?;
        let mut flag = |default: bool| arguments.next_eat().map_or(default, |value| value.to_boolean());
        let recursive = flag(false);
        let error_on_exist = flag(false);
        let force = flag(true);
        if let Some(mode) = arguments.next_eat() {
            if mode.is_number() {
                mode.coerce_to_i32(global)?;
            }
        }
        Ok(Cp {
            src,
            dest,
            recursive,
            error_on_exist,
            force,
        })
    }

    fn copy_one(&self, src: &[u8], dest: &[u8]) -> Maybe<()> {
        let exclusive = self.error_on_exist || !self.force;
        let zsrc = ZBox::from_bytes(src);
        let stat = bun_sys::lstat(&zsrc).map_err(|err| err.with_path(src))?;
        let result = if bun_sys::kind_from_mode(stat.st_mode as Mode) == bun_sys::FileKind::SymLink {
            let mut target = vec![0u8; bun_core::MAX_PATH_BYTES];
            let len = bun_sys::readlink(&zsrc, &mut target).map_err(|err| err.with_path(src))?;
            target.truncate(len);
            let zdest = ZBox::from_bytes(dest);
            match bun_sys::symlink(&ZBox::from_vec(target.clone()), &zdest) {
                Err(err) if err.get_errno() == E::EEXIST && !exclusive => {
                    bun_sys::unlink(&zdest)?;
                    bun_sys::symlink(&ZBox::from_vec(target), &zdest)
                }
                result => result,
            }
        } else {
            copy_regular_file(src, dest, exclusive)
        };
        match result {
            Err(err) if err.get_errno() == E::EEXIST && !self.error_on_exist => Ok(()),
            result => result,
        }
    }

    fn copy_directory(&self, src: &[u8], dest: &[u8]) -> Maybe<()> {
        mkdir_recursive(dest, 0o777)?;
        let entries = list_directory(&ZBox::from_bytes(src)).map_err(|err| err.with_path(src))?;
        for (name, kind) in entries {
            let (from, to) = (join(src, &name), join(dest, &name));
            if kind == bun_sys::FileKind::Directory {
                self.copy_directory(&from, &to)?;
            } else {
                self.copy_one(&from, &to)?;
            }
        }
        Ok(())
    }
}

impl Operation for Cp {
    type Out = ();
    parse_only!();
    fn run(&self) -> Maybe<()> {
        let (src, dest) = (self.src.as_bytes(), self.dest.as_bytes());
        let stat = bun_sys::lstat(z(&self.src)).map_err(|err| err.with_path(src))?;
        if bun_sys::kind_from_mode(stat.st_mode as Mode) != bun_sys::FileKind::Directory {
            return self.copy_one(src, dest);
        }
        if !self.recursive {
            return Err(bun_sys::Error::from_code(E::EISDIR, bun_sys::Tag::copyfile).with_path(src));
        }
        self.copy_directory(src, dest)
    }
    fn to_js(&self, _: &JSGlobalObject, (): (), (): ()) -> JsResult<JSValue> {
        undefined(())
    }
}
