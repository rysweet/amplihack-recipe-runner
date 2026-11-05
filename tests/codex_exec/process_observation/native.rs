//! Targeted operating-system process queries.
use super::{Identity, Process, invalid};
use std::io;

#[cfg(target_os = "linux")]
pub(super) fn observe(pid: i32) -> io::Result<Option<Process>> {
    use std::{fs, os::unix::fs::MetadataExt};
    // A missing /proc interface must not be mistaken for a missing process.
    fs::metadata("/proc/self/stat")?;
    let directory = format!("/proc/{pid}");
    let read = || -> io::Result<Process> {
        let text = fs::read_to_string(format!("{directory}/stat"))?;
        let (prefix, rest) = text
            .rsplit_once(')')
            .ok_or_else(|| invalid("missing stat command"))?;
        let number = prefix
            .split_once('(')
            .ok_or_else(|| invalid("missing stat PID"))?
            .0;
        if number.trim().parse::<i32>().ok() != Some(pid) {
            return Err(invalid("stat PID mismatch"));
        }
        let fields: Vec<_> = rest.split_whitespace().collect();
        let integer = |index: usize| -> io::Result<u64> {
            fields
                .get(index)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| invalid("invalid stat field"))
        };
        let state = match fields.first().copied() {
            Some("Z") => "zombie",
            Some("X" | "x") => "terminated",
            Some("R" | "S" | "D" | "T" | "t" | "K" | "W" | "P" | "I") => "running",
            _ => return Err(invalid("invalid stat state")),
        };
        let ppid = i32::try_from(integer(1)?).map_err(|_| invalid("invalid parent PID"))?;
        let pgid = i32::try_from(integer(2)?).map_err(|_| invalid("invalid process group"))?;
        let birth = integer(19)?;
        if birth == 0 || pgid <= 1 {
            return Err(invalid("invalid process identity"));
        }
        Ok(Process {
            identity: Identity {
                pid,
                uid: fs::metadata(&directory)?.uid(),
                pgid,
                birth: vec![birth],
            },
            ppid,
            state,
        })
    };
    // Bracket the metadata read with birth checks to detect PID reuse.
    match read().and_then(|first| {
        let second = read()?;
        if first.identity != second.identity {
            return Err(invalid("process identity changed during query"));
        }
        Ok(second)
    }) {
        Ok(process) => Ok(Some(process)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::metadata("/proc/self/stat")?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn observe(pid: i32) -> io::Result<Option<Process>> {
    // PROC_PIDTBSDINFO from libproc.h. This fixed ABI includes start timeval;
    // no process enumeration, shell command or Linux /proc fallback is used.
    #[repr(C)]
    #[derive(Default)]
    struct BsdInfo {
        flags: u32,
        status: u32,
        xstatus: u32,
        pid: u32,
        ppid: u32,
        uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        reserved: u32,
        comm: [u8; 16],
        name: [u8; 32],
        nfiles: u32,
        pgid: u32,
        pjobc: u32,
        tdev: u32,
        tpgid: u32,
        nice: i32,
        start_sec: u64,
        start_usec: u64,
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            size: i32,
        ) -> i32;
    }
    let mut info = BsdInfo::default();
    let size = std::mem::size_of::<BsdInfo>();
    assert_eq!(size, 136, "unsupported libproc BSD info layout");
    let count = unsafe {
        *libc::__error() = 0;
        proc_pidinfo(pid, 3, 0, (&mut info as *mut BsdInfo).cast(), size as i32)
    };
    if count != size as i32 {
        let error = io::Error::last_os_error();
        if count == 0 && error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(io::Error::other(format!(
            "libproc query for {pid}: {count}/{size} bytes, {error}"
        )));
    }
    if info.pid != pid as u32
        || info.start_sec == 0
        || info.start_usec >= 1_000_000
        || info.pgid <= 1
        || info.pgid > i32::MAX as u32
        || info.ppid > i32::MAX as u32
        || !(1..=7).contains(&info.status)
    {
        return Err(invalid("invalid libproc process identity or state"));
    }
    Ok(Some(Process {
        identity: Identity {
            pid,
            uid: info.uid,
            pgid: info.pgid as i32,
            birth: vec![info.start_sec, info.start_usec],
        },
        ppid: info.ppid as i32,
        state: if info.status == 5 {
            "zombie"
        } else {
            "running"
        },
    }))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn observe(_pid: i32) -> io::Result<Option<Process>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native process observer requires Linux or Darwin",
    ))
}
