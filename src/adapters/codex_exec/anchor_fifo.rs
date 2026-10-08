//! Directional FIFO endpoints opened atomically CLOEXEC before any helper fork.
use super::anchor_io::Pipe;
use anyhow::Context;
use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
};

pub(super) fn new() -> anyhow::Result<Pipe> {
    construct(
        tempfile::Builder::new()
            .prefix("codex-anchor-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?,
        |_| Ok(()),
    )
}

fn descriptor(raw: RawFd, operation: &str) -> anyhow::Result<OwnedFd> {
    if raw < 0 {
        return Err(std::io::Error::last_os_error()).context(operation.to_owned());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
fn close(fd: &mut Option<OwnedFd>, operation: &str, failures: &mut Vec<String>) {
    if let Some(fd) = fd.take()
        && unsafe { libc::close(fd.into_raw_fd()) } != 0
    {
        failures.push(format!("{operation}: {}", std::io::Error::last_os_error()));
    }
}
fn stat(fd: RawFd) -> anyhow::Result<libc::stat> {
    let mut info = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut info) } != 0 {
        return Err(std::io::Error::last_os_error()).context("Inspect Codex FIFO descriptor");
    }
    Ok(info)
}

// The callback is a deterministic test boundary; production supplies a no-op.
fn construct(
    root: tempfile::TempDir,
    mut opened: impl FnMut(&[RawFd]) -> anyhow::Result<()>,
) -> anyhow::Result<Pipe> {
    let mut directory = None;
    let mut read = None;
    let mut write = None;
    let mut created = false;
    let result = (|| {
        let path = CString::new(root.path().as_os_str().as_bytes())?;
        directory = Some(descriptor(
            unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            },
            "Open private Codex FIFO directory",
        )?);
        let dir = directory.as_ref().unwrap().as_raw_fd();
        let info = stat(dir)?;
        anyhow::ensure!(
            info.st_mode & libc::S_IFMT == libc::S_IFDIR
                && info.st_mode & 0o777 == 0o700
                && info.st_uid == unsafe { libc::geteuid() },
            "Codex FIFO directory is not private and owned"
        );
        // mkfifoat requires macOS 13; mkfifo preserves Rust's older deployment
        // floor. Open/verify through the owned directory descriptor afterward.
        let fifo = CString::new(root.path().join("endpoint").as_os_str().as_bytes())?;
        if unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } != 0 {
            return Err(std::io::Error::last_os_error()).context("Create private Codex FIFO");
        }
        created = true;
        let mut identity: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                dir,
                c"endpoint".as_ptr(),
                &mut identity,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("Inspect private Codex FIFO identity");
        }
        anyhow::ensure!(
            identity.st_mode & libc::S_IFMT == libc::S_IFIFO
                && identity.st_mode & 0o777 == 0o600
                && identity.st_uid == unsafe { libc::geteuid() },
            "Codex FIFO is not private and owned"
        );
        // Read first, nonblocking: no keeper writer and no O_RDWR masking EOF.
        for (slot, access) in [(&mut read, libc::O_RDONLY), (&mut write, libc::O_WRONLY)] {
            *slot = Some(descriptor(
                unsafe {
                    libc::openat(
                        dir,
                        c"endpoint".as_ptr(),
                        access | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW,
                    )
                },
                "Open private Codex FIFO endpoint",
            )?);
            let fd = slot.as_ref().unwrap().as_raw_fd();
            let actual = stat(fd)?;
            anyhow::ensure!(
                actual.st_dev == identity.st_dev
                    && actual.st_ino == identity.st_ino
                    && actual.st_mode & libc::S_IFMT == libc::S_IFIFO,
                "Codex FIFO endpoint identity changed"
            );
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            anyhow::ensure!(
                flags >= 0
                    && flags & libc::FD_CLOEXEC != 0
                    && status >= 0
                    && status & libc::O_NONBLOCK != 0
                    && status & libc::O_ACCMODE == access,
                "Codex FIFO endpoint flags invalid"
            );
            opened(&[dir, fd])?;
        }
        Ok(())
    })();
    let mut failures = Vec::new();
    if created
        && unsafe {
            libc::unlinkat(
                directory.as_ref().unwrap().as_raw_fd(),
                c"endpoint".as_ptr(),
                0,
            )
        } != 0
    {
        failures.push(format!(
            "Unlink private Codex FIFO: {}",
            std::io::Error::last_os_error()
        ));
    }
    close(&mut directory, "Close Codex FIFO directory", &mut failures);
    if let Err(error) = root.close() {
        failures.push(format!("Remove private Codex FIFO directory: {error}"));
    }
    if result.is_err() || !failures.is_empty() {
        close(&mut read, "Close Codex FIFO reader", &mut failures);
        close(&mut write, "Close Codex FIFO writer", &mut failures);
    }
    let cleanup = if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(failures.join("; ")))
    };
    super::combine_result(result, cleanup)?;
    Ok(Pipe {
        read: read.unwrap(),
        write: write.unwrap(),
    })
}

#[cfg(all(test, target_os = "linux"))]
#[path = "tests/anchor_fifo.rs"]
pub(super) mod tests;
