//! UDP and QUIC: not in this crate yet. Creating a UDP socket fails with EPROTONOSUPPORT, so nothing has one to pass
//! to the rest, which answer as for a socket that does not work.

use core::ffi::{c_char, c_int, c_uint, c_ushort, c_void};
use core::ptr;

use crate::{set_errno, us_loop_t};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_create_udp_socket(
    _loop: *mut us_loop_t,
    _data_cb: *const c_void,
    _drain_cb: *const c_void,
    _close_cb: *const c_void,
    _recv_error_cb: *const c_void,
    _host: *const c_char,
    _port: c_ushort,
    _options: c_int,
    err: *mut c_int,
    _user_data: *mut c_void,
) -> *mut c_void {
    if !err.is_null() {
        // SAFETY: the caller's out-int.
        unsafe { *err = libc::EPROTONOSUPPORT };
    }
    set_errno(libc::EPROTONOSUPPORT);
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn us_udp_socket_user(_socket: *mut c_void) -> *mut c_void {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn us_udp_socket_close(_socket: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn us_udp_socket_buffer_size(_socket: *mut c_void, _is_recv: c_int, _size: c_int, _out: *mut c_int) -> c_int {
    libc::EPROTONOSUPPORT
}

#[unsafe(no_mangle)]
pub extern "C" fn us_udp_packet_buffer_payload(_buf: *mut c_void, _index: c_int) -> *mut u8 {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn us_udp_packet_buffer_payload_length(_buf: *mut c_void, _index: c_int) -> c_int {
    0
}

/// No QUIC context: HTTP/3 is not offered.
#[unsafe(no_mangle)]
pub extern "C" fn us_quic_create_client_ssl_ctx() -> *mut c_void {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_is_server_authorized(_ssl: *const c_void, _hostname: *const c_char) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_parse_address(_host: *const c_char, _out: *mut c_void) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_resolved_address(_lookup: *mut c_void, _index: c_uint, _out: *mut c_void) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_is_routable(_address: *const c_void) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_read_envelope(_buffer: *mut c_void, _index: c_int, _out: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_local_address(_socket: *mut c_void, _out: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_set_dontfrag(_socket: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_ignore_receive_errors(_socket: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_stop_reading(_socket: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_enable_offload(_socket: *mut c_void) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn us_quic_send(_socket: *mut c_void, _names_source: bool, _bytes: *const u8, _messages: *const c_void, _count: usize) -> c_int {
    -1
}
