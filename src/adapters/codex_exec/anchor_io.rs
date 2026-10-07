//! Preallocated private readiness/control endpoints and bounded parent I/O.
use anyhow::Context;
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::Instant,
};
pub(super) struct Pipe {
    pub(super) read: OwnedFd,
    pub(super) write: OwnedFd,
}
impl Pipe {
    pub(super) fn new() -> anyhow::Result<Self> {
        let mut fds = [-1; 2];
        #[cfg(target_os = "linux")]
        let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        #[cfg(not(target_os = "linux"))]
        let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("Failed to create Codex anchor pipe");
        }
        let pipe = Self {
            read: unsafe { OwnedFd::from_raw_fd(fds[0]) },
            write: unsafe { OwnedFd::from_raw_fd(fds[1]) },
        };
        #[cfg(not(target_os = "linux"))]
        for fd in [&pipe.read, &pipe.write] {
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
                || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("Failed to configure Codex anchor pipe");
            }
        }
        Ok(pipe)
    }
}
pub(super) fn acknowledge(fd: &OwnedFd, deadline: Instant) -> anyhow::Result<()> {
    loop {
        anyhow::ensure!(
            Instant::now() < deadline,
            "Timed out establishing Codex anchor readiness"
        );
        let mut byte = 0u8;
        let count = unsafe { libc::read(fd.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
        if count == 1 {
            anyhow::ensure!(byte == 0xA7, "Invalid Codex anchor acknowledgment");
            return Ok(());
        }
        anyhow::ensure!(count != 0, "Codex anchor exited before readiness");
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EAGAIN) | Some(libc::EINTR)) {
            return Err(error).context("Failed to read Codex anchor acknowledgment");
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
