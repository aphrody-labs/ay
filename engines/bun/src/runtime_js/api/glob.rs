//! `Bun.Glob`, declared in src/runtime/api/Glob.bind.ts: src/runtime/api/glob.rs on `bun_js`.
//!
//! What differs from the JavaScriptCore build:
//! - `scan()` walks on the JS thread before it returns its promise: `bun_js` has no thread pool to run a job on.
//! - There is no standalone executable on Boa, so there is no `/$bunfs/` graph to walk: every walk is the file system's.

use boa_gc::{Finalize, Trace};
use bun_glob::BunGlobWalker as GlobWalker;
use bun_glob::walk;
use bun_js::bindgen::{self, IntoError, JsHeap, JsLocal, JsScope};
use bun_js::{ErrorCode, JSPromise};
use bun_paths::MAX_PATH_BYTES;
use bun_sys as syscall;

use crate::generated_bindgen::glob::{GlobScanArgument, GlobScanOptions};
use crate::sys_js::SystemError;

/// `Bun.Glob`.
#[derive(Trace, Finalize)]
pub(crate) struct Glob {
    #[unsafe_ignore_trace]
    pattern: Vec<u8>,
    #[unsafe_ignore_trace]
    shape: bun_glob::matcher::Shape,
}

pub(crate) enum ScanError<'a> {
    Sys(syscall::Error),
    Runtime(bun_core::Error),
    CwdTooLong(&'static str),
    NullBytes(&'static str, Box<[u8]>),
    Js(bindgen::Error<'a>),
}

impl<'a> IntoError<'a> for ScanError<'a> {
    #[cold]
    fn into_error(self, heap: &JsHeap<'a>) -> bindgen::Error<'a> {
        match self {
            ScanError::Sys(err) => {
                let error = SystemError::from(&err).to_error_instance(heap.global_object());
                bindgen::Error::from_value(JsLocal::new(heap, error))
            }
            ScanError::Runtime(err) => heap.error(format_args!("{}", err.name())),
            ScanError::CwdTooLong(fn_name) => heap.error(format_args!(
                "{fn_name}: invalid `cwd`, longer than {MAX_PATH_BYTES} bytes"
            )),
            ScanError::NullBytes(name, value) => heap.error_with_code(
                ErrorCode::INVALID_ARG_VALUE,
                format_args!(
                    "The argument '{name}' must be a string without null bytes. Received {}",
                    bun_core::fmt::quote(&value)
                ),
            ),
            ScanError::Js(err) => err,
        }
    }
}

struct ScanOpts {
    cwd: Option<Box<[u8]>>,
    dot: bool,
    absolute: bool,
    only_files: bool,
    follow_symlinks: bool,
    error_on_broken_symlinks: bool,
}

impl ScanOpts {
    /// `None` for the empty string, which means "not given".
    fn parse_cwd<'a>(
        cwd: &[u8],
        absolute: bool,
        fn_name: &'static str,
    ) -> Result<Option<Box<[u8]>>, ScanError<'a>> {
        if cwd.is_empty() {
            return Ok(None);
        }
        if cwd.len() > MAX_PATH_BYTES {
            return Err(ScanError::CwdTooLong(fn_name));
        }
        if bun_core::strings::contains_char(cwd, 0) {
            return Err(ScanError::NullBytes("cwd", Box::from(cwd)));
        }
        if bun_paths::is_absolute(cwd) {
            return Ok(Some(Box::from(cwd)));
        }

        let mut base_buf = bun_paths::path_buffer_pool::get();
        let base: &[u8] = match bun_core::working_directory_in_executable() {
            Some(directory) => directory,
            None if absolute => bun_sys::getcwd_z(&mut base_buf).map_err(ScanError::Sys)?,
            None => b"",
        };
        let cwd_str: Box<[u8]> = bun_paths::Path::join(&[base, cwd]).as_bytes().into();
        if cwd_str.len() > MAX_PATH_BYTES {
            return Err(ScanError::CwdTooLong(fn_name));
        }
        Ok(Some(cwd_str).filter(|cwd| !cwd.is_empty()))
    }

    fn new<'a>(
        argument: Option<GlobScanArgument<'a>>,
        fn_name: &'static str,
    ) -> Result<ScanOpts, ScanError<'a>> {
        let options = match argument {
            Some(GlobScanArgument::Options(options)) => options,
            Some(GlobScanArgument::Cwd(cwd)) => GlobScanOptions {
                cwd: Some(cwd),
                ..Default::default()
            },
            None => Default::default(),
        };
        Ok(ScanOpts {
            cwd: match options.cwd {
                Some(cwd) => Self::parse_cwd(cwd, options.absolute, fn_name)?,
                None => None,
            }
            .or_else(|| bun_core::working_directory_in_executable().map(Box::from)),
            dot: options.dot,
            absolute: options.absolute,
            only_files: options.only_files,
            follow_symlinks: options.follow_symlinks,
            error_on_broken_symlinks: options.throw_error_on_broken_symlink,
        })
    }
}

fn matched_paths_to_js<'a>(walker: &GlobWalker, heap: &JsHeap<'a>) -> bindgen::Result<'a, JsLocal<'a>> {
    let paths = walker
        .matched_paths
        .keys()
        .iter()
        .map(|path| heap.create_utf8_string(path))
        .collect::<bindgen::Result<'a, Vec<_>>>()?;
    heap.create_array_of(&paths)
}

impl Glob {
    fn make_glob_walker<'a>(
        &self,
        argument: Option<GlobScanArgument<'a>>,
        fn_name: &'static str,
    ) -> Result<Box<GlobWalker>, ScanError<'a>> {
        if bun_core::strings::contains_char(&self.pattern, 0) {
            return Err(ScanError::NullBytes("pattern", Box::from(&self.pattern[..])));
        }
        let opts = ScanOpts::new(argument, fn_name)?;
        let result = match &opts.cwd {
            Some(cwd) => walk::GlobWalker::<walk::SyscallAccessor, false>::init_with_cwd(
                &self.pattern,
                cwd,
                opts.dot,
                opts.absolute,
                opts.follow_symlinks,
                opts.error_on_broken_symlinks,
                opts.only_files,
                None,
            ),
            None => walk::GlobWalker::<walk::SyscallAccessor, false>::init(
                &self.pattern,
                opts.dot,
                opts.absolute,
                opts.follow_symlinks,
                opts.error_on_broken_symlinks,
                opts.only_files,
                None,
            ),
        };
        result
            .map_err(ScanError::Runtime)?
            .map(Box::new)
            .map_err(ScanError::Sys)
    }

    pub(crate) fn constructor(pattern: Vec<u8>) -> Self {
        Glob {
            shape: bun_glob::matcher::Shape::of(&pattern),
            pattern,
        }
    }

    pub(crate) fn r#match(&self, str: &[u8]) -> bool {
        bun_glob::matcher::match_prepared::<{ cfg!(windows) }>(
            &self.pattern,
            &self.shape,
            str,
            bun_glob::Dots::Match,
        )
        .matches()
    }

    /// The paths, or the walk's error, as a settled promise.
    pub(crate) fn scan<'a>(
        &self,
        cx: &mut JsScope<'a>,
        options: Option<GlobScanArgument<'a>>,
    ) -> Result<JsLocal<'a>, ScanError<'a>> {
        let mut walker = self.make_glob_walker(options, "scan")?;
        let global = cx.heap.global_object();
        let walked = match walker.walk() {
            Ok(Ok(())) => matched_paths_to_js(&walker, &cx.heap).map_err(ScanError::Js)?,
            Ok(Err(err)) => {
                let reason = SystemError::from(&err).to_error_instance(global);
                return Ok(JsLocal::new(&cx.heap, JSPromise::rejected_promise_value(global, reason)));
            }
            Err(err) => {
                let reason = bun_js::JSValue::from_utf8(err.name().as_bytes());
                return Ok(JsLocal::new(&cx.heap, JSPromise::rejected_promise_value(global, reason)));
            }
        };
        Ok(JsLocal::new(
            &cx.heap,
            JSPromise::resolved_promise_value(global, walked.to_js()),
        ))
    }

    pub(crate) fn scan_sync<'a>(
        &self,
        heap: &JsHeap<'a>,
        options: Option<GlobScanArgument<'a>>,
    ) -> Result<JsLocal<'a>, ScanError<'a>> {
        let mut walker = self.make_glob_walker(options, "scanSync")?;
        walker
            .walk()
            .map_err(ScanError::Runtime)?
            .map_err(ScanError::Sys)?;
        matched_paths_to_js(&walker, heap).map_err(ScanError::Js)
    }
}
