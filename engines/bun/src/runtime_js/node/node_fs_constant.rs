//! `process.binding('constants').fs`, which is `fs.constants`, and `.os`, which node:fs reads errnos from:
//! `processBindingConstantsGetFs` and `processBindingConstantsGetOs` of src/jsc/bindings/ProcessBindingConstants.cpp,
//! with the values of this platform's libc.

use bun_js::{JSGlobalObject, JSObject, JSValue};

/// The names and values, in the order C++ puts them.
pub fn constants() -> Vec<(&'static str, f64)> {
    let mut list: Vec<(&'static str, f64)> = vec![
        ("UV_FS_SYMLINK_DIR", 1.0),
        ("UV_FS_SYMLINK_JUNCTION", 2.0),
        ("O_RDONLY", f64::from(libc::O_RDONLY)),
        ("O_WRONLY", f64::from(libc::O_WRONLY)),
        ("O_RDWR", f64::from(libc::O_RDWR)),
        ("UV_DIRENT_UNKNOWN", 0.0),
        ("UV_DIRENT_FILE", 1.0),
        ("UV_DIRENT_DIR", 2.0),
        ("UV_DIRENT_LINK", 3.0),
        ("UV_DIRENT_FIFO", 4.0),
        ("UV_DIRENT_SOCKET", 5.0),
        ("UV_DIRENT_CHAR", 6.0),
        ("UV_DIRENT_BLOCK", 7.0),
    ];
    let mode = |m: libc::mode_t| f64::from(m as u32);
    list.extend([
        ("S_IFMT", mode(libc::S_IFMT)),
        ("S_IFREG", mode(libc::S_IFREG)),
        ("S_IFDIR", mode(libc::S_IFDIR)),
        ("S_IFCHR", mode(libc::S_IFCHR)),
        ("S_IFBLK", mode(libc::S_IFBLK)),
        ("S_IFIFO", mode(libc::S_IFIFO)),
        ("S_IFLNK", mode(libc::S_IFLNK)),
        ("S_IFSOCK", mode(libc::S_IFSOCK)),
        ("O_CREAT", f64::from(libc::O_CREAT)),
        ("O_EXCL", f64::from(libc::O_EXCL)),
        ("UV_FS_O_FILEMAP", 0.0),
        ("O_NOCTTY", f64::from(libc::O_NOCTTY)),
        ("O_TRUNC", f64::from(libc::O_TRUNC)),
        ("O_APPEND", f64::from(libc::O_APPEND)),
        ("O_DIRECTORY", f64::from(libc::O_DIRECTORY)),
    ]);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    list.push(("O_NOATIME", f64::from(libc::O_NOATIME)));
    list.extend([
        ("O_NOFOLLOW", f64::from(libc::O_NOFOLLOW)),
        ("O_SYNC", f64::from(libc::O_SYNC)),
        ("O_DSYNC", f64::from(libc::O_DSYNC)),
    ]);
    #[cfg(target_os = "macos")]
    list.push(("O_SYMLINK", f64::from(libc::O_SYMLINK)));
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    list.push(("O_DIRECT", f64::from(libc::O_DIRECT)));
    list.extend([
        ("O_NONBLOCK", f64::from(libc::O_NONBLOCK)),
        ("S_IRWXU", mode(libc::S_IRWXU)),
        ("S_IRUSR", mode(libc::S_IRUSR)),
        ("S_IWUSR", mode(libc::S_IWUSR)),
        ("S_IXUSR", mode(libc::S_IXUSR)),
        ("S_IRWXG", mode(libc::S_IRWXG)),
        ("S_IRGRP", mode(libc::S_IRGRP)),
        ("S_IWGRP", mode(libc::S_IWGRP)),
        ("S_IXGRP", mode(libc::S_IXGRP)),
        ("S_IRWXO", mode(libc::S_IRWXO)),
        ("S_IROTH", mode(libc::S_IROTH)),
        ("S_IWOTH", mode(libc::S_IWOTH)),
        ("S_IXOTH", mode(libc::S_IXOTH)),
        ("F_OK", f64::from(libc::F_OK)),
        ("R_OK", f64::from(libc::R_OK)),
        ("W_OK", f64::from(libc::W_OK)),
        ("X_OK", f64::from(libc::X_OK)),
        ("UV_FS_COPYFILE_EXCL", 1.0),
        ("COPYFILE_EXCL", 1.0),
        ("UV_FS_COPYFILE_FICLONE", 2.0),
        ("COPYFILE_FICLONE", 2.0),
        ("UV_FS_COPYFILE_FICLONE_FORCE", 4.0),
        ("COPYFILE_FICLONE_FORCE", 4.0),
        ("EXTENSIONLESS_FORMAT_JAVASCRIPT", 0.0),
        ("EXTENSIONLESS_FORMAT_WASM", 1.0),
    ]);
    list
}

/// The names of `processBindingConstantsGetOs`'s `errno` that are POSIX's, in its order.
const ERRNO_NAMES: [&str; 79] = [
    "E2BIG", "EACCES", "EADDRINUSE", "EADDRNOTAVAIL", "EAFNOSUPPORT", "EAGAIN", "EALREADY", "EBADF", "EBADMSG",
    "EBUSY", "ECANCELED", "ECHILD", "ECONNABORTED", "ECONNREFUSED", "ECONNRESET", "EDEADLK", "EDESTADDRREQ", "EDOM",
    "EDQUOT", "EEXIST", "EFAULT", "EFBIG", "EHOSTUNREACH", "EIDRM", "EILSEQ", "EINPROGRESS", "EINTR", "EINVAL", "EIO",
    "EISCONN", "EISDIR", "ELOOP", "EMFILE", "EMLINK", "EMSGSIZE", "EMULTIHOP", "ENAMETOOLONG", "ENETDOWN",
    "ENETRESET", "ENETUNREACH", "ENFILE", "ENOBUFS", "ENODATA", "ENODEV", "ENOENT", "ENOEXEC", "ENOLCK", "ENOLINK",
    "ENOMEM", "ENOMSG", "ENOPROTOOPT", "ENOSPC", "ENOSR", "ENOSTR", "ENOSYS", "ENOTCONN", "ENOTDIR", "ENOTEMPTY",
    "ENOTSOCK", "ENOTSUP", "ENOTTY", "ENXIO", "EOPNOTSUPP", "EOVERFLOW", "EPERM", "EPIPE", "EPROTO",
    "EPROTONOSUPPORT", "EPROTOTYPE", "ERANGE", "EROFS", "ESPIPE", "ESRCH", "ESTALE", "ETIME", "ETIMEDOUT", "ETXTBSY",
    "EWOULDBLOCK", "EXDEV",
];

/// `errno` of `process.binding('constants').os`: each of those this platform has.
pub fn errno() -> Vec<(&'static str, f64)> {
    ERRNO_NAMES
        .iter()
        .filter_map(|&name| {
            let errno: bun_sys::SystemErrno = name.parse().ok()?;
            Some((name, f64::from(errno as u16)))
        })
        .collect()
}

fn object_of(global: &JSGlobalObject, constants: Vec<(&'static str, f64)>) -> JSObject {
    let object = JSObject::create(global);
    object.as_engine().set_prototype(None);
    for (name, value) in constants {
        object.put(global, name, JSValue::js_number(value));
    }
    object
}

/// The object, with a null prototype as C++'s.
pub fn create(global: &JSGlobalObject) -> JSValue {
    object_of(global, constants()).to_js()
}

/// `signals` of `process.binding('constants').os`, in `processBindingConstantsGetOs`'s order.
pub fn signals() -> Vec<(&'static str, f64)> {
    let mut list = vec![
        ("SIGHUP", libc::SIGHUP),
        ("SIGINT", libc::SIGINT),
        ("SIGQUIT", libc::SIGQUIT),
        ("SIGILL", libc::SIGILL),
        ("SIGTRAP", libc::SIGTRAP),
        ("SIGABRT", libc::SIGABRT),
        ("SIGIOT", libc::SIGIOT),
        ("SIGBUS", libc::SIGBUS),
        ("SIGFPE", libc::SIGFPE),
        ("SIGKILL", libc::SIGKILL),
        ("SIGUSR1", libc::SIGUSR1),
        ("SIGSEGV", libc::SIGSEGV),
        ("SIGUSR2", libc::SIGUSR2),
        ("SIGPIPE", libc::SIGPIPE),
        ("SIGALRM", libc::SIGALRM),
        ("SIGTERM", libc::SIGTERM),
        ("SIGCHLD", libc::SIGCHLD),
    ];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    list.push(("SIGSTKFLT", libc::SIGSTKFLT));
    list.extend([
        ("SIGCONT", libc::SIGCONT),
        ("SIGSTOP", libc::SIGSTOP),
        ("SIGTSTP", libc::SIGTSTP),
        ("SIGTTIN", libc::SIGTTIN),
        ("SIGTTOU", libc::SIGTTOU),
        ("SIGURG", libc::SIGURG),
        ("SIGXCPU", libc::SIGXCPU),
        ("SIGXFSZ", libc::SIGXFSZ),
        ("SIGVTALRM", libc::SIGVTALRM),
        ("SIGPROF", libc::SIGPROF),
        ("SIGWINCH", libc::SIGWINCH),
        ("SIGIO", libc::SIGIO),
    ]);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    list.extend([("SIGPOLL", libc::SIGPOLL), ("SIGPWR", libc::SIGPWR)]);
    #[cfg(target_os = "macos")]
    list.push(("SIGINFO", libc::SIGINFO));
    list.push(("SIGSYS", libc::SIGSYS));
    list.into_iter().map(|(name, value)| (name, f64::from(value))).collect()
}

/// `process.binding('constants').os`: `errno`, `signals`, `priority` and `dlopen`.
pub fn create_os(global: &JSGlobalObject) -> JSValue {
    let os = object_of(global, vec![("UV_UDP_REUSEADDR", 4.0)]);
    let mut dlopen = vec![
        ("RTLD_LAZY", f64::from(libc::RTLD_LAZY)),
        ("RTLD_NOW", f64::from(libc::RTLD_NOW)),
        ("RTLD_GLOBAL", f64::from(libc::RTLD_GLOBAL)),
        ("RTLD_LOCAL", f64::from(libc::RTLD_LOCAL)),
    ];
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    dlopen.push(("RTLD_DEEPBIND", f64::from(libc::RTLD_DEEPBIND)));
    os.put(global, "dlopen", object_of(global, dlopen).to_js());
    os.put(global, "errno", object_of(global, errno()).to_js());
    os.put(global, "signals", object_of(global, signals()).to_js());
    let priority = vec![
        ("PRIORITY_LOW", 19.0),
        ("PRIORITY_BELOW_NORMAL", 10.0),
        ("PRIORITY_NORMAL", 0.0),
        ("PRIORITY_ABOVE_NORMAL", -7.0),
        ("PRIORITY_HIGH", -14.0),
        ("PRIORITY_HIGHEST", -20.0),
    ];
    os.put(global, "priority", object_of(global, priority).to_js());
    os.to_js()
}
