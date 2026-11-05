//! Native, test-only observations of synthetic descendants. Query errors remain
//! errors; only a confirmed missing process establishes absence.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Identity {
    pid: i32,
    uid: u32,
    pgid: i32,
    birth: Vec<u64>,
}

#[derive(Debug, PartialEq)]
struct Process {
    identity: Identity,
    ppid: i32,
    state: &'static str,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[path = "process_observation/native.rs"]
mod native;
use native::observe;

fn publish(root: &Path, name: &str, value: &Value) -> io::Result<()> {
    let temporary = root.join(format!("{name}.tmp"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(value.to_string().as_bytes())?;
    drop(file);
    fs::rename(temporary, root.join(name))
}

fn record(identity: &Identity, boundary: &str, observed: io::Result<Option<Process>>) -> Value {
    let (state, current, error) = match observed {
        Ok(Some(process)) => (process.state, Some(process.identity), None),
        Ok(None) => ("absent", None, None),
        Err(error) => ("unavailable", None, Some(error.to_string())),
    };
    json!({"schema_version":1, "backend":if cfg!(target_os="macos") {"darwin_native"} else {"linux_proc"},
        "boundary":boundary, "identity":identity, "observed_identity":current, "state":state, "error":error})
}

fn pid_file(root: &Path, name: &str) -> io::Result<i32> {
    let value = fs::read_to_string(root.join(name))?;
    let pid = value
        .parse::<i32>()
        .map_err(|_| invalid("invalid fixture PID record"))?;
    if pid <= 1 {
        return Err(invalid("fixture PID must be greater than one"));
    }
    Ok(pid)
}

/// Observe the ready descendant while the launcher keeps it alive. The launcher
/// waits for atomic publication inside the adapter's deadline, never before it.
pub(super) fn watch_ready(root: &Path, done: Arc<AtomicBool>) -> JoinHandle<io::Result<()>> {
    let root = root.to_owned();
    thread::spawn(move || {
        loop {
            match pid_file(&root, "launcher.pid") {
                Ok(launcher) => {
                    let pid = pid_file(&root, "descendant.pid")?;
                    let process =
                        observe(pid)?.ok_or_else(|| invalid("ready descendant is absent"))?;
                    if process.ppid != launcher
                        || process.identity.pgid != launcher
                        || process.identity.uid != unsafe { libc::geteuid() }
                        || process.state != "running"
                    {
                        return Err(invalid("ready descendant ownership or state mismatch"));
                    }
                    return publish(
                        &root,
                        "descendant-live.json",
                        &record(&process.identity, "descendant_ready", observe(pid)),
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if done.load(Ordering::Acquire) {
                return Err(invalid("adapter returned before descendant readiness"));
            }
            thread::sleep(Duration::from_millis(1));
        }
    })
}

/// Capture the boundary before joining the observer or performing any cleanup.
pub(super) fn capture_return(root: &Path) -> io::Result<()> {
    let live: Value = serde_json::from_slice(&fs::read(root.join("descendant-live.json"))?)?;
    let identity: Identity = serde_json::from_value(live["identity"].clone())?;
    let observed = observe(identity.pid);
    publish(
        root,
        "descendant-return.json",
        &record(&identity, "adapter_return", observed),
    )
}

/// Fallback signals only an unchanged synthetic identity owned by this user.
/// The recorded adapter-return assertion still fails after successful cleanup.
fn cleanup_if_running(root: &Path, identity: &Identity) -> io::Result<()> {
    if identity.pid != pid_file(root, "descendant.pid")?
        || identity.pgid != pid_file(root, "launcher.pid")?
    {
        return Err(invalid("fallback cleanup ownership is unverified"));
    }
    cleanup_identity(identity)
}

fn cleanup_identity(identity: &Identity) -> io::Result<()> {
    if identity.uid != unsafe { libc::geteuid() }
        || identity.pid <= 1
        || identity.pgid <= 1
        || identity.pgid == unsafe { libc::getpgrp() }
    {
        return Err(invalid("fallback cleanup identity is unverified"));
    }
    if let Some(process) = observe(identity.pid)? {
        if process.identity != *identity {
            return Err(invalid("fallback cleanup refused changed identity"));
        }
        if process.state == "running" {
            // Birth comparison and kill are separate system calls. The small
            // remaining reuse race is not an atomic kernel process handle.
            if unsafe { libc::kill(identity.pid, libc::SIGKILL) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) struct CancellationObservation([Identity; 2]);

#[cfg(target_os = "linux")]
impl CancellationObservation {
    pub(super) fn capture(root: &Path, runner: i32) -> io::Result<Self> {
        let launcher = pid_file(root, "launcher.pid")?;
        let descendant = pid_file(root, "descendant.pid")?;
        let mut identities = Vec::new();
        for (pid, parent) in [(launcher, runner), (descendant, launcher)] {
            let process =
                observe(pid)?.ok_or_else(|| invalid("ready cancellation process is absent"))?;
            if process.identity.uid != unsafe { libc::geteuid() }
                || process.identity.pgid != launcher
                || process.ppid != parent
                || process.state != "running"
            {
                return Err(invalid("cancellation process ownership or state mismatch"));
            }
            identities.push(process.identity);
        }
        Ok(Self(identities.try_into().unwrap()))
    }

    pub(super) fn cleanup(&self) -> io::Result<()> {
        // Signal each recorded process separately; no authority is inferred for
        // other members of a group after the runner has returned.
        for identity in self.0.iter().rev() {
            cleanup_identity(identity)?;
        }
        Ok(())
    }

    pub(super) fn assert_stopped(&self) {
        // Observe both at the return boundary before attempting any fallback.
        let observations = self.0.each_ref().map(|identity| observe(identity.pid));
        for (identity, observed) in self.0.iter().zip(observations) {
            match observed.expect("cancellation return observation failed") {
                None => {}
                Some(process) => {
                    assert_eq!(
                        process.identity, *identity,
                        "cancellation PID identity changed"
                    );
                    if process.state == "running" {
                        self.cleanup()
                            .expect("verified cancellation fallback cleanup failed");
                        panic!("owned cancellation process alive at runner return");
                    }
                }
            }
        }
    }
}

fn read_observation(root: &Path, name: &str) -> Value {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(root.join(name)).expect("private observation is required");
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let bytes = fs::read(root.join(name)).expect("native process observation is required");
    let observation: Value = serde_json::from_slice(&bytes).expect("complete process observation");
    assert_eq!(observation["schema_version"], 1);
    #[cfg(target_os = "macos")]
    assert_eq!(observation["backend"], "darwin_native");
    #[cfg(target_os = "linux")]
    assert_eq!(observation["backend"], "linux_proc");
    observation
}

pub(super) fn assert_stopped_at_adapter_return(root: &Path) {
    let pid: u32 = fs::read_to_string(root.join("descendant.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(pid > 1);
    let live = read_observation(root, "descendant-live.json");
    assert_eq!(live["boundary"], "descendant_ready");
    assert_eq!(
        live["state"], "running",
        "known live control must be observed"
    );
    let identity = &live["identity"];
    assert_eq!(identity["pid"], pid);
    assert_eq!(identity["uid"], unsafe { libc::geteuid() });
    assert!(identity["pgid"].as_u64().is_some_and(|pgid| pgid > 1));
    let birth = identity["birth"]
        .as_array()
        .expect("native process birth identity");
    assert!(!birth.is_empty());
    assert!(birth.iter().all(|part| part.as_u64().is_some()));
    assert!(birth.iter().any(|part| part.as_u64().unwrap() > 0));
    assert_eq!(live["observed_identity"], *identity);

    let returned = read_observation(root, "descendant-return.json");
    assert_eq!(returned["boundary"], "adapter_return");
    assert_eq!(returned["identity"], *identity);
    match returned["state"].as_str().expect("explicit process state") {
        "absent" => assert!(returned["observed_identity"].is_null()),
        "zombie" | "terminated" => assert_eq!(returned["observed_identity"], *identity),
        // Read failures, parse failures, PID reuse and unavailable platform
        // interfaces must fail. None of them establishes process absence.
        _ => {
            let identity: Identity = serde_json::from_value(identity.clone()).unwrap();
            cleanup_if_running(root, &identity).expect("verified fallback cleanup failed");
            panic!("descendant was live or its termination could not be verified at adapter return")
        }
    }
}
