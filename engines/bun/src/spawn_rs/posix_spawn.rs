//! `posix_spawn_bun` of src/jsc/bindings/bun-spawn.cpp, in Rust: the child resets its signals, joins the cgroup,
//! makes its session or process group, applies the file actions, drops credentials, marks every other fd
//! close-on-exec, and execs. It runs on `fork()` and reports a failed exec through a close-on-exec pipe, which is
//! the C++'s macOS path: `vfork()` returns twice, which Rust code cannot be written for. Between fork and exec the
//! child calls only async-signal-safe libc functions on memory that the parent prepared.

use core::ffi::{c_char, c_int, c_ulong};
use core::sync::atomic::{AtomicBool, Ordering};

use bun_core::spawn_ffi::{BunSpawnRequest, FileActionType};

/// `bun_pending_fork_t`, `bun_sys::close_on_fork::PendingFork`.
#[repr(C)]
struct PendingFork {
    fds: *const c_int,
    fd_count: usize,
    has_closed_fds: AtomicBool,
}

unsafe extern "C" {
    static environ: *const *const c_char;
    fn pthread_setcancelstate(state: c_int, oldstate: *mut c_int) -> c_int;
}

/// glibc's and musl's `PTHREAD_CANCEL_DISABLE`.
const PTHREAD_CANCEL_DISABLE: c_int = 1;

fn errno() -> c_int {
    // SAFETY: the calling thread's errno.
    unsafe { *libc::__errno_location() }
}

fn raw_exit(status: c_int) -> ! {
    // SAFETY: exit_group takes an int; libc's exit paths take locks that other threads of the parent may hold.
    unsafe {
        libc::syscall(libc::SYS_exit_group, status);
    }
    loop {}
}

fn max_fd(start: c_int, end: c_int) -> c_int {
    // SAFETY: sysconf reads a limit.
    let mut maxfd = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) } as c_int;
    if !(0..=65536).contains(&maxfd) {
        maxfd = 65536;
    }
    if end >= start && end < c_int::MAX {
        maxfd = maxfd.min(end + 1);
    }
    maxfd
}

/// close_range(CLOSE_RANGE_CLOEXEC), or a loop over the fds where the kernel has none.
fn cloexec_from(start: c_int) {
    const CLOSE_RANGE_CLOEXEC: c_ulong = 1 << 2;
    // SAFETY: close_range takes integers.
    if unsafe { libc::syscall(libc::SYS_close_range, start as c_ulong, c_ulong::from(u32::MAX), CLOSE_RANGE_CLOEXEC) } == 0 {
        return;
    }
    for fd in start..max_fd(start, c_int::MAX) {
        // SAFETY: fcntl on any fd number.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// The child's side: never returns.
///
/// # Safety
/// Called in the child of `fork()`, with the parent's arguments.
unsafe fn start_child(
    path: *const c_char,
    request: &BunSpawnRequest,
    pending_fork: *mut PendingFork,
    argv: *const *const c_char,
    envp: *const *const c_char,
    oldmask: &libc::sigset_t,
    errpipe_write: c_int,
) -> ! {
    let fail_with = |err: c_int| -> ! {
        // SAFETY: a write of an int to the pipe the parent reads.
        unsafe { libc::write(errpipe_write, (&raw const err).cast(), core::mem::size_of::<c_int>()) };
        raw_exit(127)
    };
    let fail = || -> ! { fail_with(errno()) };
    // SAFETY: every call is an async-signal-safe syscall on the parent's prepared memory.
    unsafe {
        if !pending_fork.is_null() {
            for i in 0..(*pending_fork).fd_count {
                libc::close(*(*pending_fork).fds.add(i));
            }
            (*pending_fork).has_closed_fds.store(true, Ordering::Release);
        }

        let mut default: libc::sigaction = core::mem::zeroed();
        default.sa_sigaction = libc::SIG_DFL;
        // NSIG is 65.
        for sig in 1..65 {
            if sig == libc::SIGSYS {
                continue;
            }
            libc::sigaction(sig, &default, core::ptr::null_mut());
        }

        if request.cgroup_fd >= 0 {
            let procs = libc::openat(request.cgroup_fd, c"cgroup.procs".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            if procs < 0 || libc::write(procs, b"0".as_ptr().cast(), 1) != 1 {
                // Negative: the cgroup failed, not the spawn proper.
                fail_with(-errno());
            }
            libc::close(procs);
        }

        if request.detached || request.pty_slave_fd >= 0 {
            libc::setsid();
        } else if request.new_process_group {
            libc::setpgid(0, 0);
        }
        if request.linux_pdeathsig != 0 {
            libc::prctl(libc::PR_SET_PDEATHSIG, request.linux_pdeathsig as c_ulong, 0, 0, 0);
        }
        if request.pty_slave_fd >= 0 {
            libc::ioctl(request.pty_slave_fd, libc::TIOCSCTTY, 0);
        }

        if !request.chdir_buf.is_null() && libc::chdir(request.chdir_buf) != 0 {
            fail();
        }

        let mut current_max_fd = 0;
        let actions = if request.actions.len == 0 {
            &[][..]
        } else {
            core::slice::from_raw_parts(request.actions.ptr, request.actions.len)
        };
        for action in actions {
            match action.kind {
                FileActionType::Close => {
                    libc::close(action.fds[0]);
                }
                FileActionType::Dup2 => {
                    if action.fds[0] == action.fds[1] {
                        // dup2 onto itself leaves FD_CLOEXEC: clear it, or exec closes the fd.
                        let mask = libc::fcntl(action.fds[0], libc::F_GETFD, 0);
                        if mask == -1 || libc::fcntl(action.fds[0], libc::F_SETFD, mask & !libc::FD_CLOEXEC) == -1 {
                            fail();
                        }
                    } else if libc::dup2(action.fds[0], action.fds[1]) == -1 {
                        fail();
                    }
                    current_max_fd = current_max_fd.max(action.fds[1]);
                }
                FileActionType::Open => {
                    let opened = libc::open(action.path, action.flags, action.mode as libc::c_uint);
                    if opened == -1 {
                        fail();
                    }
                    if opened != action.fds[0] {
                        if libc::dup2(opened, action.fds[0]) == -1 {
                            libc::close(opened);
                            fail();
                        }
                        if libc::close(opened) != 0 {
                            fail();
                        }
                    }
                    current_max_fd = current_max_fd.max(action.fds[0]);
                }
                FileActionType::None => {}
            }
        }

        if request.set_uid || request.set_gid {
            libc::syscall(libc::SYS_setgroups, 0, core::ptr::null::<libc::gid_t>());
        }
        if request.set_gid && libc::syscall(libc::SYS_setgid, request.gid as libc::gid_t) != 0 {
            fail();
        }
        if request.set_uid && libc::syscall(libc::SYS_setuid, request.uid as libc::uid_t) != 0 {
            fail();
        }
        // Changing credentials clears PR_SET_PDEATHSIG.
        if request.linux_pdeathsig != 0 && (request.set_uid || request.set_gid) {
            libc::prctl(libc::PR_SET_PDEATHSIG, request.linux_pdeathsig as c_ulong, 0, 0, 0);
        }

        let envp = if envp.is_null() { environ } else { envp };
        // The error pipe is above every target, and close-on-exec too.
        cloexec_from(current_max_fd + 1);

        libc::sigprocmask(libc::SIG_SETMASK, oldmask, core::ptr::null_mut());
        libc::sigaction(libc::SIGSYS, &default, core::ptr::null_mut());
        libc::execve(path, argv.cast(), envp.cast());
        fail()
    }
}

/// 0 and `*pid` for a child that exec'd, else the errno of what failed (negated if joining the cgroup failed).
#[unsafe(no_mangle)]
unsafe extern "C" fn posix_spawn_bun(
    pid: *mut c_int,
    path: *const c_char,
    request: *const BunSpawnRequest,
    pending_fork: *mut PendingFork,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> isize {
    // SAFETY: the caller's request, argv and envp, which stay alive and unchanged until this returns.
    unsafe {
        let request = &*request;
        let mut errpipe = [0 as c_int; 2];
        if libc::pipe2(errpipe.as_mut_ptr(), libc::O_CLOEXEC) == -1 {
            return errno() as isize;
        }
        // With fd 0, 1 or 2 closed the write end can have a number that a file action targets.
        let actions = if request.actions.len == 0 {
            &[][..]
        } else {
            core::slice::from_raw_parts(request.actions.ptr, request.actions.len)
        };
        let highest_target = actions
            .iter()
            .map(|a| if a.kind == FileActionType::Dup2 { a.fds[1] } else { a.fds[0] })
            .fold(libc::STDERR_FILENO, c_int::max);
        if errpipe[1] <= highest_target {
            let above = libc::fcntl(errpipe[1], libc::F_DUPFD_CLOEXEC, highest_target + 1);
            if above != -1 {
                libc::close(errpipe[1]);
                errpipe[1] = above;
            }
        }

        let mut blockall: libc::sigset_t = core::mem::zeroed();
        let mut oldmask: libc::sigset_t = core::mem::zeroed();
        libc::sigfillset(&mut blockall);
        // A seccomp trap while SIGSYS is blocked kills the process.
        libc::sigdelset(&mut blockall, libc::SIGSYS);
        libc::sigprocmask(libc::SIG_SETMASK, &blockall, &mut oldmask);
        let mut cs: c_int = 0;
        pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &mut cs);

        let child = libc::fork();
        if child == 0 {
            libc::close(errpipe[0]);
            start_child(path, request, pending_fork, argv, envp, &oldmask, errpipe[1]);
        }
        let fork_errno = errno();
        libc::close(errpipe[1]);

        let res: isize = if child == -1 {
            libc::close(errpipe[0]);
            fork_errno as isize
        } else {
            let mut child_err: c_int = 0;
            let n = loop {
                let n = libc::read(errpipe[0], (&raw mut child_err).cast(), core::mem::size_of::<c_int>());
                if n == -1 && errno() == libc::EINTR {
                    continue;
                }
                break n;
            };
            let read_errno = errno();
            libc::close(errpipe[0]);
            if n == 0 {
                if !pid.is_null() {
                    *pid = child;
                }
                0
            } else {
                libc::waitpid(child, core::ptr::null_mut(), 0);
                if n == core::mem::size_of::<c_int>() as isize {
                    child_err as isize
                } else if n == -1 {
                    read_errno as isize
                } else {
                    libc::EIO as isize
                }
            }
        };

        libc::sigprocmask(libc::SIG_SETMASK, &oldmask, core::ptr::null_mut());
        pthread_setcancelstate(cs, core::ptr::null_mut());
        res
    }
}
