//! Preallocated private readiness/control endpoints and bounded parent I/O.
use anyhow::Context;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
#[cfg(all(test, target_os = "linux"))]
thread_local! {
    pub(super) static TEST_FIFO: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
use std::{
    os::fd::{AsRawFd, OwnedFd},
    time::Instant,
};
pub(super) struct Pipe {
    pub(super) read: OwnedFd,
    pub(super) write: OwnedFd,
}
impl Pipe {
    pub(super) fn new() -> anyhow::Result<Self> {
        #[cfg(all(test, target_os = "linux"))]
        if TEST_FIFO.get() {
            return super::anchor_fifo::new();
        }
        #[cfg(not(target_os = "linux"))]
        return super::anchor_fifo::new();
        #[cfg(target_os = "linux")]
        Self::anonymous()
    }
    #[cfg(target_os = "linux")]
    fn anonymous() -> anyhow::Result<Self> {
        let mut fds = [-1; 2];
        let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("Failed to create Codex anchor pipe");
        }
        let pipe = Self {
            read: unsafe { OwnedFd::from_raw_fd(fds[0]) },
            write: unsafe { OwnedFd::from_raw_fd(fds[1]) },
        };
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
