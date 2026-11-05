//! Bounded, nonblocking-safe reads from one trusted private directory.
use super::invalid;
use std::{
    ffi::CString,
    fs,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    time::Instant,
};

pub(super) const RECORD_LIMIT: u64 = 4096;

/// One opened private directory and a shared deadline for bounded record reads.
/// Records supply candidate PIDs only; they never confer signalling authority.
pub(super) struct PrivateRecords {
    directory: fs::File,
    deadline: Instant,
}

impl PrivateRecords {
    pub(super) fn open(root: &Path, deadline: Instant) -> io::Result<Self> {
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(invalid("fixture directory is not private"));
        }
        Ok(Self {
            directory,
            deadline,
        })
    }

    pub(super) fn check_deadline(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "record observation deadline",
            ));
        }
        Ok(())
    }

    /// Return one bounded, descriptor-validated record or an explicit error.
    /// O_NONBLOCK prevents FIFO opening from stalling before type validation.
    pub(super) fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        self.check_deadline()?;
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(invalid("record name must be one filename component"));
        }
        let name = CString::new(name).map_err(|_| invalid("invalid record name"))?;
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(invalid(
                "record must be a private, singly linked regular file",
            ));
        }
        if metadata.len() > RECORD_LIMIT {
            return Err(invalid("record exceeds size limit"));
        }
        let mut bytes = Vec::new();
        file.take(RECORD_LIMIT + 1).read_to_end(&mut bytes)?;
        self.check_deadline()?;
        if bytes.len() as u64 > RECORD_LIMIT {
            return Err(invalid("record exceeds size limit"));
        }
        Ok(bytes)
    }

    pub(super) fn pid_file(&self, name: &str) -> io::Result<i32> {
        let bytes = self.read(name)?;
        // A PID is at most ten decimal digits. No whitespace or sign is valid.
        if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
            return Err(invalid("invalid fixture PID record"));
        }
        let pid = std::str::from_utf8(&bytes)
            .unwrap()
            .parse::<i32>()
            .map_err(|_| invalid("invalid fixture PID record"))?;
        if pid <= 1 {
            return Err(invalid("fixture PID must be greater than one"));
        }
        Ok(pid)
    }
}
