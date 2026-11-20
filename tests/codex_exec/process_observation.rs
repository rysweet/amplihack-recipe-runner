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
    time::{Duration, Instant},
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
#[path = "process_observation/records.rs"]
mod records;
use records::PrivateRecords;
#[path = "process_observation/ownership.rs"]
mod ownership;
#[cfg(target_os = "linux")]
pub(super) use ownership::CancellationObservation;
pub(super) use ownership::{ChildGuard, Ownership, RetainedOwnership, SpawnedWorker};

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

fn record(identity: &Identity, boundary: &str, observed: Option<Process>) -> Value {
    let (state, current) = match observed {
        Some(process) => (process.state, Some(process.identity)),
        None => ("absent", None),
    };
    json!({"schema_version":1, "backend":if cfg!(target_os="macos") {"darwin_native"} else {"linux_proc"},
        "boundary":boundary, "identity":identity, "observed_identity":current, "state":state, "error":null})
}

/// The parent supplies its freshly spawned worker identity. The launcher waits
/// inside the unchanged adapter deadline until verified readiness is published.
pub(super) fn watch_ready(
    root: &Path,
    worker: SpawnedWorker,
    done: Arc<AtomicBool>,
) -> JoinHandle<io::Result<RetainedOwnership>> {
    let root = root.to_owned();
    thread::spawn(move || {
        let records = PrivateRecords::open(&root, Instant::now() + Duration::from_secs(8))?;
        loop {
            match records.pid_file("launcher.pid") {
                Ok(_) => {
                    let evidence = Ownership::capture(&records, &worker)?;
                    return complete_ready(&root, evidence, observe);
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

// Retain cleanup authority before observation or publication can fail.
fn complete_ready(
    root: &Path,
    evidence: Ownership,
    observation: impl FnOnce(i32) -> io::Result<Option<Process>>,
) -> io::Result<RetainedOwnership> {
    let retained = RetainedOwnership(evidence);
    publish(
        root,
        "descendant-live.json",
        &record(
            &retained.descendant,
            "descendant_ready",
            observation(retained.descendant.pid)?,
        ),
    )?;
    publish(
        root,
        "supervisor-release.json",
        &json!({"ownership_retained":true}),
    )?;
    Ok(retained)
}

/// Always join the observer and release retained authority before reporting an
/// adapter failure. Untrusted records can never nominate a cleanup target.
pub(super) fn finish_observed(
    child: &mut ChildGuard,
    root: &Path,
    done: Arc<AtomicBool>,
    observer: Option<JoinHandle<io::Result<RetainedOwnership>>>,
    watchdog: Duration,
) -> io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + watchdog;
    let status = (|| loop {
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        if Instant::now() >= deadline {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "adapter exceeded watchdog",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    })();
    if status.is_err() {
        child.stop();
    }
    done.store(true, Ordering::Release);
    let observed = observer.map(|observer| observer.join());
    // Dropping the joined result also cleans verified ownership on this path.
    let status = status?;
    if let Some(observed) = observed {
        let ownership = observed.map_err(|_| invalid("fixture observer panicked"))??;
        ownership.validate_return(root)?;
        ownership.cleanup()?;
    }
    Ok(status)
}

/// Capture at adapter return. This untrusted record conveys no cleanup authority.
pub(super) fn capture_return(root: &Path) -> io::Result<()> {
    let records = PrivateRecords::open(root, Instant::now() + Duration::from_secs(1))?;
    let live: Value = serde_json::from_slice(&records.read("descendant-live.json")?)?;
    let identity: Identity = serde_json::from_value(live["identity"].clone())?;
    if live["schema_version"] != 1
        || live["boundary"] != "descendant_ready"
        || live["state"] != "running"
        || live["observed_identity"] != live["identity"]
        || !live["error"].is_null()
        || identity.pid <= 1
        || identity.pgid <= 1
        || identity.uid != unsafe { libc::geteuid() }
        || identity.birth.is_empty()
        || identity.birth.len() > 2
        || identity.birth.iter().all(|part| *part == 0)
    {
        return Err(invalid("invalid live process observation"));
    }
    let observed = observe(identity.pid)?;
    publish(
        root,
        "descendant-return.json",
        &record(&identity, "adapter_return", observed),
    )
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

fn read_observation(root: &Path, name: &str) -> Value {
    let records = PrivateRecords::open(root, Instant::now() + Duration::from_secs(1))
        .expect("private observation directory is required");
    let bytes = records
        .read(name)
        .expect("validated native process observation is required");
    let observation: Value = serde_json::from_slice(&bytes).expect("complete process observation");
    assert_eq!(observation["schema_version"], 1);
    #[cfg(target_os = "macos")]
    assert_eq!(observation["backend"], "darwin_native");
    #[cfg(target_os = "linux")]
    assert_eq!(observation["backend"], "linux_proc");
    observation
}

pub(super) fn assert_stopped_at_adapter_return(root: &Path) {
    let records = PrivateRecords::open(root, Instant::now() + Duration::from_secs(1)).unwrap();
    let pid = records.pid_file("descendant.pid").unwrap();
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
            panic!("descendant was live or its termination could not be verified at adapter return")
        }
    }
}

#[path = "process_observation/security_tests.rs"]
mod security_tests;

#[path = "process_observation/record_tests.rs"]
mod record_tests;
