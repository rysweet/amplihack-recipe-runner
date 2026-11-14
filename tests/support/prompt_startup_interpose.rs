//! Linux-only fault fixture: acknowledge the anchor fork before parent readiness.
//! Built as a standalone preload library; it never changes production behavior.
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::atomic::{AtomicBool, Ordering},
};

unsafe extern "C" {
    fn dlsym(handle: *mut std::ffi::c_void, name: *const std::ffi::c_char)
    -> *mut std::ffi::c_void;
    fn __errno_location() -> *mut i32;
}

static GATED: AtomicBool = AtomicBool::new(false);

#[unsafe(no_mangle)]
unsafe extern "C" fn fork() -> i32 {
    let real: unsafe extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(dlsym((-1isize) as *mut _, c"fork".as_ptr())) };
    let pid = unsafe { real() };
    if pid > 0 && !GATED.swap(true, Ordering::SeqCst) {
        let path = std::env::var_os("PROMPT_STARTUP_GATE").unwrap();
        let mut wire = UnixStream::connect(path).unwrap();
        writeln!(wire, "ANCHOR_FORKED {pid}").unwrap();
        // EOF also releases the worker if its controller unwinds.
        let _ = wire.read_exact(&mut [0u8]);
    }
    pid
}

#[unsafe(no_mangle)]
unsafe extern "C" fn kill(pid: i32, signal: i32) -> i32 {
    if pid < -1
        && GATED.load(Ordering::SeqCst)
        && std::env::var_os("PROMPT_DENY_GROUP_SIGNALS").is_some()
    {
        unsafe { *__errno_location() = 1 }; // EPERM: retirement cannot be certified.
        return -1;
    }
    let real: unsafe extern "C" fn(i32, i32) -> i32 =
        unsafe { std::mem::transmute(dlsym((-1isize) as *mut _, c"kill".as_ptr())) };
    unsafe { real(pid, signal) }
}
