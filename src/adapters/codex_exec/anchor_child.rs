//! Post-fork helper: raw operations only, no Rust destructors, allocation or locks.
use std::os::fd::RawFd;

/// The caller prepared every value before fork. Every failure exits without unwinding.
pub(super) unsafe fn run(group: i32, control: RawFd, ready: RawFd, lifetime_ms: Option<u64>) -> ! {
    unsafe {
        if libc::setpgid(0, group) != 0 {
            libc::_exit(111);
        }
        // No inherited user handlers may run in the helper. All masks are child-local.
        let mut action: libc::sigaction = std::mem::zeroed();
        libc::sigemptyset(&mut action.sa_mask);
        for signal in 1..signal_limit() {
            if signal == libc::SIGKILL || signal == libc::SIGSTOP {
                continue;
            }
            action.sa_sigaction = if matches!(signal, libc::SIGINT | libc::SIGTERM | libc::SIGPIPE)
            {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                // Linux reserves 32/33 for its threading implementation.
                #[cfg(target_os = "linux")]
                if signal == 32 || signal == 33 {
                    continue;
                }
                if errno() == libc::EINVAL {
                    continue;
                }
                libc::_exit(112);
            }
        }
        let mut mask = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        if libc::sigprocmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut()) != 0 {
            libc::_exit(113);
        }
        if !isolate(control, ready) {
            libc::_exit(114);
        }
        let mut started: libc::timespec = std::mem::zeroed();
        if libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut started) != 0 {
            libc::_exit(116);
        }
        let byte = 0xA7u8;
        if libc::write(ready, (&byte as *const u8).cast(), 1) != 1 {
            libc::_exit(115);
        }
        libc::close(ready);
        loop {
            let mut now: libc::timespec = std::mem::zeroed();
            if libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) != 0 {
                libc::_exit(116);
            }
            let elapsed = (now.tv_sec - started.tv_sec)
                .saturating_mul(1000)
                .saturating_add((now.tv_nsec - started.tv_nsec).div_euclid(1_000_000))
                .max(0) as u64;
            if lifetime_ms.is_some_and(|limit| elapsed >= limit) {
                libc::_exit(117);
            }
            let remaining =
                lifetime_ms.map_or(1000, |limit| limit.saturating_sub(elapsed).min(1000)) as i32;
            let mut poll = libc::pollfd {
                fd: control,
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            };
            let rc = libc::poll(&mut poll, 1, remaining);
            if rc < 0 {
                if errno() == libc::EINTR {
                    continue;
                }
                libc::_exit(118);
            }
            if rc > 0 {
                let mut value = 0u8;
                let read = libc::read(control, (&mut value as *mut u8).cast(), 1);
                if read == 0 {
                    libc::_exit(0);
                }
                if read > 0 || !matches!(errno(), libc::EAGAIN | libc::EINTR) {
                    libc::_exit(119);
                }
            }
        }
    }
}
#[cfg(target_os = "linux")]
fn signal_limit() -> i32 {
    65
}
#[cfg(not(target_os = "linux"))]
fn signal_limit() -> i32 {
    129
}
#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "hurd",
    target_os = "redox",
    target_os = "emscripten",
    target_os = "l4re"
))]
unsafe fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}
#[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
unsafe fn errno() -> i32 {
    unsafe { *libc::__error() }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn isolate(control: RawFd, ready: RawFd) -> bool {
    let low = control.min(ready) as u32;
    let high = control.max(ready) as u32;
    // flags=0 closes the actual ranges, including descriptors above lowered limits.
    for (first, last) in [
        (0, low.checked_sub(1)),
        (low + 1, high.checked_sub(1)),
        (high + 1, Some(u32::MAX)),
    ] {
        if let Some(last) = last
            && first <= last
            && unsafe { libc::syscall(libc::SYS_close_range, first, last, 0u32) } != 0
        {
            return false;
        }
    }
    true
}

#[cfg(target_vendor = "apple")]
unsafe fn isolate(control: RawFd, ready: RawFd) -> bool {
    // Bound the actual post-fork kernel descriptor TABLE, independent of RLIMIT.
    // XNU proc_pidfdlist(NULL) returns (fd_nfiles + 20) * sizeof(proc_fdinfo).
    // This child is single-threaded and opens no descriptors after this query.
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    let width = std::mem::size_of::<libc::proc_fdinfo>() as i32;
    if bytes <= 0 || bytes % width != 0 {
        return false;
    }
    let bound = bytes / width;
    // Refuse excessively large tables within the startup budget rather than leak.
    if bound > 262144 || bound <= control.max(ready) {
        return false;
    }
    for fd in 0..bound {
        if fd != control
            && fd != ready
            && unsafe { libc::close(fd) } != 0
            && unsafe { errno() } != libc::EBADF
        {
            return false;
        }
    }
    true
}

#[cfg(all(
    not(any(target_os = "linux", target_os = "android")),
    not(target_vendor = "apple")
))]
unsafe fn isolate(control: RawFd, ready: RawFd) -> bool {
    // Retain the two protocol endpoints, close the complete tail with closefrom,
    // then close each lower descriptor. This child creates no additional handles.
    #[cfg(not(any(
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "aix",
        target_os = "hurd"
    )))]
    unsafe extern "C" {
        fn closefrom(lowfd: libc::c_int);
    }
    let high = control.max(ready);
    if high > 262144 {
        return false;
    }
    #[cfg(target_os = "freebsd")]
    unsafe {
        libc::closefrom(high + 1);
    }
    #[cfg(any(target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly"))]
    if unsafe { libc::closefrom(high + 1) } != 0 {
        return false;
    }
    #[cfg(target_os = "aix")]
    if unsafe { libc::fcntl(high + 1, libc::F_CLOSEM) } != 0 {
        return false;
    }
    #[cfg(target_os = "hurd")]
    if unsafe { libc::close_range((high + 1) as u32, u32::MAX, 0) } != 0 {
        return false;
    }
    #[cfg(not(any(
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "aix",
        target_os = "hurd"
    )))]
    unsafe {
        closefrom(high + 1);
    }
    for fd in 0..high {
        if fd != control
            && fd != ready
            && unsafe { libc::close(fd) } != 0
            && unsafe { errno() } != libc::EBADF
        {
            return false;
        }
    }
    true
}

#[cfg(any(target_os = "netbsd", target_os = "openbsd", target_os = "android"))]
unsafe fn errno() -> i32 {
    unsafe { *libc::__errno() }
}
#[cfg(any(target_os = "solaris", target_os = "illumos"))]
unsafe fn errno() -> i32 {
    unsafe { *libc::___errno() }
}

#[cfg(target_os = "aix")]
unsafe fn errno() -> i32 {
    unsafe { *libc::_Errno() }
}
#[cfg(target_os = "haiku")]
unsafe fn errno() -> i32 {
    unsafe { *libc::_errnop() }
}
#[cfg(target_os = "nto")]
unsafe fn errno() -> i32 {
    unsafe { *libc::__get_errno_ptr() }
}
