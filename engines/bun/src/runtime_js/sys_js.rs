//! `bun_sys::Error` as JavaScript sees it, as `bun_sys_jsc` makes it: `SystemError__toErrorInstance` in
//! src/jsc/bindings/bindings.cpp, with the message Node's `UVException` has.

use bun_js::{JSGlobalObject, JSValue};

/// What C++'s `SystemError` holds. A field that is `None` is not a property of the error.
#[derive(Clone, Debug, Default)]
pub struct SystemError {
    /// As Node reports it: the negated errno.
    pub errno: i32,
    pub code: Option<&'static str>,
    pub message: String,
    pub path: Option<Vec<u8>>,
    pub dest: Option<Vec<u8>>,
    pub syscall: Option<&'static str>,
    pub fd: Option<i32>,
}

impl SystemError {
    /// The error of a JavaScript value, with the properties in the order C++ puts them.
    pub fn to_error_instance(&self, global: &JSGlobalObject) -> JSValue {
        let error = global.create_error_instance(format_args!("{}", self.message));
        let text = |bytes: &[u8]| JSValue::from_utf8(bytes);
        if let Some(code) = self.code {
            error.put(global, "code", text(code.as_bytes()));
        }
        if let Some(path) = &self.path {
            error.put(global, "path", text(path));
        }
        if let Some(dest) = &self.dest {
            error.put(global, "dest", text(dest));
        }
        if let Some(fd) = self.fd {
            error.put(global, "fd", JSValue::js_number_from_int32(fd));
        }
        if let Some(syscall) = self.syscall {
            error.put(global, "syscall", text(syscall.as_bytes()));
        }
        error.put(global, "errno", JSValue::js_number_from_int32(self.errno));
        error
    }

    /// This error with `path`, as `bun_sys::Error::with_path` before the conversion.
    pub fn with_path(mut self, path: &[u8]) -> SystemError {
        if !path.is_empty() {
            self.path = Some(path.to_vec());
        }
        self
    }
}

/// `bun_sys::Error::to_system_error`: `ENOENT: no such file or directory, open 'path'`.
impl From<&bun_sys::Error> for SystemError {
    fn from(err: &bun_sys::Error) -> SystemError {
        let syscall: &'static str = err.syscall.into();
        let looked_up = (err.errno != 0).then(|| {
            err.uv_code_label()
                .unwrap_or(("EUNKNOWN", "unknown error"))
        });
        let mut message = String::new();
        if let Some((code, _)) = looked_up {
            message.push_str(code);
            message.push_str(": ");
        }
        message.push_str(looked_up.map_or("Unknown Error", |(_, label)| label));
        message.push_str(", ");
        message.push_str(syscall);
        let lossy = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        if !err.path.is_empty() {
            message.push_str(&format!(" '{}'", lossy(&err.path)));
            if !err.dest.is_empty() {
                message.push_str(&format!(" -> '{}'", lossy(&err.dest)));
            }
        }
        SystemError {
            errno: -i32::from(err.errno),
            code: looked_up.map(|(code, _)| code),
            message,
            path: (!err.path.is_empty()).then(|| err.path.to_vec()),
            dest: (!err.dest.is_empty()).then(|| err.dest.to_vec()),
            syscall: Some(syscall),
            fd: (err.fd != bun_sys::Fd::INVALID).then(|| err.fd.uv()),
        }
    }
}

impl From<bun_sys::Error> for SystemError {
    fn from(err: bun_sys::Error) -> SystemError {
        SystemError::from(&err)
    }
}
