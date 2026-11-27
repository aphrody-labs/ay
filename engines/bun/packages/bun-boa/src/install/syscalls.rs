//! src/jsc/bindings/c-bindings.cpp's `sys_preadv2` and `sys_pwritev2`, which `bun_sys` reads and writes pipes with.

use core::ffi::c_int;

const RWF_NOWAIT: libc::c_long = 0x0000_0008;

/// Always RWF_NOWAIT, whatever `flags` says, as in c-bindings.cpp. On 64-bit kernels the high half of the position
/// is ignored.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn sys_preadv2(fd: c_int, iov: *const libc::iovec, iovcnt: c_int, offset: i64, _flags: u32) -> isize {
    // SAFETY: the caller's iovecs.
    unsafe { libc::syscall(libc::SYS_preadv2, fd, iov, iovcnt, offset, offset >> 32, RWF_NOWAIT) as isize }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn sys_pwritev2(fd: c_int, iov: *const libc::iovec, iovcnt: c_int, offset: i64, flags: u32) -> isize {
    #[cfg(target_pointer_width = "64")]
    let (pos_l, pos_h) = (offset as libc::c_ulong, 0 as libc::c_ulong);
    #[cfg(target_pointer_width = "32")]
    let (pos_l, pos_h) = ((offset & 0xffff_ffff) as libc::c_ulong, ((offset as u64) >> 32) as libc::c_ulong);
    // SAFETY: the caller's iovecs.
    unsafe { libc::syscall(libc::SYS_pwritev2, fd, iov, iovcnt, pos_l, pos_h, flags as libc::c_ulong) as isize }
}
