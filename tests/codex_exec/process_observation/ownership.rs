//! Live ancestry is the only source of fallback signalling authority.
use super::{Identity, PrivateRecords, cleanup_identity, invalid, observe};
use serde_json::Value;
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};

/// Trusted anchor captured from Child::id(), never from fixture records.
pub(crate) struct SpawnedWorker(pub(super) Identity);
impl SpawnedWorker {
    pub(crate) fn capture(pid: u32) -> io::Result<Self> {
        let pid = i32::try_from(pid).map_err(|_| invalid("invalid spawned PID"))?;
        let process = observe(pid)?.ok_or_else(|| invalid("spawned worker is absent"))?;
        if process.ppid != std::process::id() as i32
            || process.identity.uid != unsafe { libc::geteuid() }
            || process.state != "running"
        {
            return Err(invalid("spawned worker ownership mismatch"));
        }
        Ok(Self(process.identity))
    }
}

/// In-memory evidence captured while the complete ownership chain is live.
/// No deserialisation path exists for this authority.
#[derive(Clone)]
pub(crate) struct Ownership {
    pub(super) worker: Identity,
    pub(super) launcher: Identity,
    pub(super) descendant: Identity,
}

impl Ownership {
    pub(super) fn capture(records: &PrivateRecords, worker: &SpawnedWorker) -> io::Result<Self> {
        let launcher = records.pid_file("launcher.pid")?;
        let descendant = records.pid_file("descendant.pid")?;
        let current = observe(worker.0.pid)?.ok_or_else(|| invalid("worker is absent"))?;
        if current.identity != worker.0 || current.state != "running" {
            return Err(invalid("worker birth identity or state mismatch"));
        }
        let parent = observe(launcher)?.ok_or_else(|| invalid("launcher is absent"))?;
        let child = observe(descendant)?.ok_or_else(|| invalid("descendant is absent"))?;
        if parent.ppid != worker.0.pid
            || parent.identity.pgid != launcher
            || child.ppid != launcher
            || child.identity.pgid != launcher
            || parent.identity.uid != worker.0.uid
            || child.identity.uid != worker.0.uid
            || parent.state != "running"
            || child.state != "running"
        {
            return Err(invalid("fixture ancestry or state mismatch"));
        }
        // Bracket the relationship queries. A parent exit/reparent or identity
        // change during capture invalidates the evidence rather than repairing it.
        for before in [&current, &parent, &child] {
            if observe(before.identity.pid)?.as_ref() != Some(before) {
                return Err(invalid("fixture ancestry changed during capture"));
            }
        }
        records.check_deadline()?;
        Ok(Self {
            worker: worker.0.clone(),
            launcher: parent.identity,
            descendant: child.identity,
        })
    }

    /// Bind boundary receipts to the identity retained by the spawning parent.
    /// A receipt may report state, but cannot nominate another cleanup target.
    pub(crate) fn validate_return(&self, root: &Path) -> io::Result<()> {
        let records = PrivateRecords::open(root, Instant::now() + Duration::from_secs(1))?;
        for (name, boundary) in [
            ("descendant-live.json", "descendant_ready"),
            ("descendant-return.json", "adapter_return"),
        ] {
            let value: Value = serde_json::from_slice(&records.read(name)?)?;
            let identity: Identity = serde_json::from_value(value["identity"].clone())?;
            let state = value["state"]
                .as_str()
                .ok_or_else(|| invalid("missing observation state"))?;
            if identity != self.descendant
                || value["schema_version"] != 1
                || value["boundary"] != boundary
                || !value["error"].is_null()
                || (boundary == "descendant_ready" && state != "running")
                || !matches!(state, "running" | "absent" | "zombie" | "terminated")
            {
                return Err(invalid(
                    "boundary observation does not match retained ownership",
                ));
            }
            if state == "absent" {
                if !value["observed_identity"].is_null() {
                    return Err(invalid("contradictory absent observation"));
                }
            } else {
                let observed: Identity =
                    serde_json::from_value(value["observed_identity"].clone())?;
                if observed != identity {
                    return Err(invalid("boundary observed identity changed"));
                }
            }
        }
        records.check_deadline()
    }

    pub(crate) fn cleanup(&self) -> io::Result<()> {
        // Validate the retained evidence before any signal. Reparenting after
        // verified capture is allowed, but births, user and group must match.
        if self.worker.pid <= 1
            || self.worker.birth.is_empty()
            || self.launcher.pid != self.launcher.pgid
            || self.descendant.pgid != self.launcher.pid
            || self.worker.uid != self.launcher.uid
            || self.worker.uid != self.descendant.uid
        {
            return Err(invalid("fallback ownership evidence is contradictory"));
        }
        for identity in [&self.worker, &self.launcher, &self.descendant] {
            if let Some(current) = observe(identity.pid)?
                && current.identity != *identity
            {
                return Err(invalid(
                    "fallback cleanup refused changed ownership identity",
                ));
            }
        }
        cleanup_identity(&self.descendant)?;
        cleanup_identity(&self.launcher)
    }
}

/// A direct Child handle authorises cleanup even when identity capture fails.
pub(crate) struct ChildGuard(Option<std::process::Child>);
impl ChildGuard {
    pub(crate) fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    pub(crate) fn stop(&mut self) {
        let Some(child) = self.0.as_mut() else { return };
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(error) = child.kill() {
            eprintln!("direct fixture child kill failed: {error}");
        }
        if let Err(error) = child.wait() {
            eprintln!("direct fixture child wait failed: {error}");
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn wait_with_output(mut self) -> io::Result<std::process::Output> {
        use std::io::Read;
        let status = self.wait()?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut pipe) = self.stdout.take() {
            pipe.read_to_end(&mut stdout)?;
        }
        if let Some(mut pipe) = self.stderr.take() {
            pipe.read_to_end(&mut stderr)?;
        }
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }
}
impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Only verified in-memory ancestry can construct this cleanup guard.
pub(crate) struct RetainedOwnership(pub(super) Ownership);
impl std::ops::Deref for RetainedOwnership {
    type Target = Ownership;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for RetainedOwnership {
    fn drop(&mut self) {
        if let Err(error) = self.0.cleanup() {
            eprintln!("verified fixture cleanup failed: {error}");
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct CancellationObservation(RetainedOwnership);

#[cfg(target_os = "linux")]
impl CancellationObservation {
    pub(crate) fn capture(
        root: &Path,
        worker: &SpawnedWorker,
        deadline: Instant,
    ) -> io::Result<Self> {
        let records = PrivateRecords::open(root, deadline)?;
        let observation = Self(RetainedOwnership(Ownership::capture(&records, worker)?));
        super::publish(
            root,
            "supervisor-release.json",
            &serde_json::json!({"ownership_retained":true}),
        )?;
        Ok(observation)
    }

    pub(crate) fn launcher_pid(&self) -> i32 {
        self.0.launcher.pid
    }

    pub(crate) fn cleanup(&self) -> io::Result<()> {
        self.0.cleanup()
    }

    pub(crate) fn assert_stopped(&self) {
        // Observe both at the return boundary before attempting any fallback.
        let observations =
            [&self.0.launcher, &self.0.descendant].map(|identity| observe(identity.pid));
        for (identity, observed) in [&self.0.launcher, &self.0.descendant]
            .into_iter()
            .zip(observations)
        {
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
