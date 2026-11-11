//! Private local delegation. Only the root writer can finalize an attempt.
//! Each attachment is a random, generation-bound capability for one subtree.
use super::{
    model::*,
    private_fs::{self},
    store::Store,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    thread::JoinHandle,
};

pub const ATTACHMENT_ENV: &str = "AMPLIHACK_RECOVERY_ATTACHMENT";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    socket: PathBuf,
    token: String,
    pub prefix: String,
    generation: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Begin {
        probe: Probe,
    },
    Commit {
        record: Record,
    },
    Attach {
        prefix: String,
    },
    Outcome {
        outcome: Outcome,
    },
    Enter {
        recipe: String,
        discriminator: String,
    },
    Fresh,
    Renew,
    Retire {
        prefix: String,
    },
    Close,
    Finalize {
        recipe: String,
        failed: Vec<String>,
        outcome: Outcome,
    },
    Report,
    Shutdown,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    token: String,
    generation: u64,
    operation: Operation,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    value: serde_json::Value,
    error: Option<String>,
}
struct Grant {
    prefix: String,
    generation: u64,
    root: bool,
}
pub struct Server {
    client: Client,
    thread: Option<JoinHandle<Result<()>>>,
    _socket_dir: Option<tempfile::TempDir>,
}
fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() as u64 <= private_fs::MAX_DOCUMENT,
        "oversized recovery frame"
    );
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}
fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    ensure!(
        len as u64 <= private_fs::MAX_DOCUMENT,
        "oversized recovery frame"
    );
    let mut b = vec![0; len];
    stream.read_exact(&mut b)?;
    Ok(b)
}
impl Server {
    pub fn start(store: Store) -> Result<Self> {
        // TMPDIR is supplied by the caller's private runtime; a short basename
        // leaves room for Unix socket addresses. Never place the socket in history.
        #[cfg(target_os = "linux")]
        let (listener, socket, directory) = {
            use std::os::linux::net::SocketAddrExt;
            let name = format!("recipe-recovery-{}", uuid::Uuid::new_v4());
            let address = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
            (
                UnixListener::bind_addr(&address)?,
                PathBuf::from(format!("@{name}")),
                None,
            )
        };
        #[cfg(not(target_os = "linux"))]
        let (listener, socket, directory) = {
            let directory = tempfile::Builder::new().prefix("rr-").tempdir()?;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
            Directory::open(directory.path(), false)?;
            let socket = directory.path().join("s");
            let listener = UnixListener::bind(&socket)?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
            (listener, socket, Some(directory))
        };
        let client = Client {
            socket: socket.clone(),
            token: uuid::Uuid::new_v4().to_string(),
            prefix: String::new(),
            generation: 0,
        };
        let mut grants = BTreeMap::new();
        grants.insert(
            client.token.clone(),
            Grant {
                prefix: String::new(),
                generation: 0,
                root: true,
            },
        );
        let thread = std::thread::spawn(move || serve(listener, socket, store, grants));
        Ok(Self {
            client,
            thread: Some(thread),
            _socket_dir: directory,
        })
    }
    pub fn client(&self) -> Client {
        self.client.clone()
    }
    pub fn finish_result(
        mut self,
        result: &crate::models::RecipeResult,
    ) -> Result<(Report, Option<Location>)> {
        let outcome = if result.success {
            Outcome::Success
        } else if super::replay::usage_limit(result) {
            Outcome::UsageLimit
        } else {
            Outcome::Failure
        };
        let failed = result
            .step_results
            .iter()
            .filter(|r| r.status == crate::models::StepStatus::Failed)
            .map(|r| r.step_id.clone())
            .collect();
        let cause = serde_json::from_value(self.client.call(Operation::Finalize {
            recipe: result.recipe_name.clone(),
            failed,
            outcome,
        })?)?;
        let report = self.client.report()?;
        self.client.call(Operation::Shutdown)?;
        self.thread
            .take()
            .expect("server thread")
            .join()
            .map_err(|_| anyhow::anyhow!("recovery writer panicked"))??;
        Ok((report, cause))
    }
    pub fn finish(mut self, outcome: Outcome) -> Result<Report> {
        self.client.outcome(outcome)?;
        let report = self.client.report()?;
        self.client.call(Operation::Shutdown)?;
        self.thread
            .take()
            .expect("server thread")
            .join()
            .map_err(|_| anyhow::anyhow!("recovery writer panicked"))??;
        Ok(report)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // Shutdown is a control operation, never a success/limit assertion.
            if self.client.call(Operation::Shutdown).is_ok() {
                if !matches!(thread.join(), Ok(Ok(()))) {
                    eprintln!("recovery writer did not shut down cleanly");
                }
            } else {
                eprintln!("recovery writer shutdown unavailable");
            }
        }
    }
}
impl Client {
    fn call(&self, operation: Operation) -> Result<serde_json::Value> {
        let mut stream =
            connect(&self.socket).map_err(|_| anyhow::anyhow!("recovery writer unavailable"))?;
        write_frame(
            &mut stream,
            &super::private_fs::serialize(&Request {
                token: self.token.clone(),
                generation: self.generation,
                operation,
            })?,
        )?;
        let response: Response = private_fs::parse(&read_frame(&mut stream)?)?;
        if response.error.is_some() {
            anyhow::bail!("recovery operation rejected or could not be published");
        }
        Ok(response.value)
    }
    pub fn from_environment() -> Result<Option<Self>> {
        std::env::var_os(ATTACHMENT_ENV)
            .map(|v| {
                v.into_string()
                    .map_err(|_| anyhow::anyhow!("invalid recovery attachment"))
                    .and_then(|s| private_fs::parse(s.as_bytes()))
            })
            .transpose()
    }
    pub fn environment(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
    pub fn begin(&self, probe: Probe) -> Result<Option<Record>> {
        Ok(serde_json::from_value(
            self.call(Operation::Begin { probe })?,
        )?)
    }
    pub fn commit(&self, record: Record) -> Result<()> {
        self.call(Operation::Commit { record })?;
        Ok(())
    }
    pub fn attach(&self, prefix: String) -> Result<Self> {
        Ok(serde_json::from_value(
            self.call(Operation::Attach { prefix })?,
        )?)
    }
    pub fn report(&self) -> Result<Report> {
        Ok(serde_json::from_value(self.call(Operation::Report)?)?)
    }
    /// Retire the delegated invocation's credential when its process returns.
    pub fn invocation_guard(&self) -> InvocationGuard {
        InvocationGuard(self.clone())
    }
    pub fn enter_invocation(&self, recipe: String, discriminator: String) -> Result<Self> {
        Ok(serde_json::from_value(self.call(Operation::Enter {
            recipe,
            discriminator,
        })?)?)
    }
    pub fn fresh_subtree(&self) -> Result<()> {
        self.call(Operation::Fresh)?;
        Ok(())
    }
    pub fn renew(&self) -> Result<Self> {
        Ok(serde_json::from_value(self.call(Operation::Renew)?)?)
    }
    pub fn retire(&self, prefix: String) -> Result<()> {
        self.call(Operation::Retire { prefix })?;
        Ok(())
    }
    pub fn close(&self) -> Result<()> {
        self.call(Operation::Close)?;
        Ok(())
    }
    pub fn outcome(&self, outcome: Outcome) -> Result<()> {
        self.call(Operation::Outcome { outcome })?;
        Ok(())
    }
}
fn authorized(grant: &Grant, path: &str) -> bool {
    grant.root || path.starts_with(&format!("{}/", grant.prefix))
}
fn serve(
    listener: UnixListener,
    socket: PathBuf,
    mut store: Store,
    mut grants: BTreeMap<String, Grant>,
) -> Result<()> {
    let mut invocation_counts: BTreeMap<(String, String), u64> = BTreeMap::new();
    for incoming in listener.incoming() {
        let mut stream = incoming?;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let mut credentials = libc::ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            };
            let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            // SAFETY: live socket and correctly sized writable credentials.
            ensure!(
                unsafe {
                    libc::getsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_PEERCRED,
                        (&mut credentials as *mut libc::ucred).cast(),
                        &mut length,
                    )
                } == 0,
                "cannot verify recovery peer"
            );
            // SAFETY: geteuid has no preconditions.
            if credentials.uid != unsafe { libc::geteuid() } {
                continue;
            }
        }
        let request = read_frame(&mut stream).and_then(|b| private_fs::parse::<Request>(&b));
        let mut shutdown = false;
        let value = (|| -> Result<serde_json::Value> {
            let request = request?;
            let grant = grants
                .get(&request.token)
                .ok_or_else(|| anyhow::anyhow!("unknown attachment"))?;
            ensure!(
                grant.generation == request.generation,
                "stale recovery generation"
            );
            match request.operation {
                Operation::Begin { probe } => {
                    ensure!(
                        authorized(grant, &probe.location.invocation_path),
                        "outside invocation subtree"
                    );
                    Ok(serde_json::to_value(store.begin(&probe)?)?)
                }
                Operation::Commit { record } => {
                    ensure!(
                        authorized(grant, &record.probe.location.invocation_path),
                        "outside invocation subtree"
                    );
                    store.commit(record)?;
                    Ok(serde_json::Value::Null)
                }
                Operation::Attach { prefix } => {
                    ensure!(
                        authorized(grant, &prefix) && store.attachment_admitted(&prefix),
                        "outside admitted delegation subtree"
                    );
                    ensure!(grants.len() < 10_000, "too many recovery attachments");
                    let generation = store.tree.generation;
                    let client = Client {
                        socket: socket.clone(),
                        token: uuid::Uuid::new_v4().to_string(),
                        prefix: prefix.clone(),
                        generation,
                    };
                    grants.insert(
                        client.token.clone(),
                        Grant {
                            prefix,
                            generation,
                            root: false,
                        },
                    );
                    Ok(serde_json::to_value(client)?)
                }
                Operation::Outcome { outcome } => {
                    // Descendants may report an active blockage. Their success
                    // and other failures never finalize or establish root eligibility.
                    ensure!(
                        grant.root || outcome == Outcome::UsageLimit || outcome == Outcome::Running,
                        "child cannot finalize root"
                    );
                    if grant.root {
                        store.outcome(outcome)?;
                    } else {
                        store.scoped_outcome(&grant.prefix, outcome)?;
                    }
                    Ok(serde_json::Value::Null)
                }
                Operation::Enter {
                    recipe,
                    discriminator,
                } => {
                    ensure!(!grant.root, "delegated entry requires a subtree capability");
                    super::private_fs::component(&recipe)?;
                    ensure!(
                        discriminator.len() == 64
                            && discriminator.bytes().all(|b| b.is_ascii_hexdigit()),
                        "invalid invocation discriminator"
                    );
                    ensure!(
                        grants.len() < 10_000 && invocation_counts.len() < 10_000,
                        "too many delegated invocations"
                    );
                    let key = super::model::fingerprint(&(recipe, &discriminator))?;
                    let ordinal = invocation_counts
                        .entry((request.token.clone(), key.clone()))
                        .or_insert(0);
                    let prefix = format!("{}/call-{}-{}", grant.prefix, &key[..16], ordinal);
                    *ordinal = ordinal
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("invocation count exhausted"))?;
                    let generation = grant.generation;
                    let client = Client {
                        socket: socket.clone(),
                        token: uuid::Uuid::new_v4().to_string(),
                        prefix: prefix.clone(),
                        generation,
                    };
                    grants.insert(
                        client.token.clone(),
                        Grant {
                            prefix,
                            generation,
                            root: false,
                        },
                    );
                    Ok(serde_json::to_value(client)?)
                }
                Operation::Fresh => {
                    store.fresh_subtree(&grant.prefix)?;
                    Ok(serde_json::Value::Null)
                }
                Operation::Renew => {
                    ensure!(
                        !grant.root,
                        "root attachment cannot be renewed by child protocol"
                    );
                    let prefix = grant.prefix.clone();
                    let generation = grant
                        .generation
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("attachment generation exhausted"))?;
                    grants.retain(|_, g| {
                        g.root
                            || !(g.prefix == prefix || g.prefix.starts_with(&format!("{prefix}/")))
                    });
                    let client = Client {
                        socket: socket.clone(),
                        token: uuid::Uuid::new_v4().to_string(),
                        prefix: prefix.clone(),
                        generation,
                    };
                    grants.insert(
                        client.token.clone(),
                        Grant {
                            prefix,
                            generation,
                            root: false,
                        },
                    );
                    store.retry_subtree(&client.prefix)?;
                    Ok(serde_json::to_value(client)?)
                }
                Operation::Retire { prefix } => {
                    ensure!(
                        authorized(grant, &prefix) && store.attachment_admitted(&prefix),
                        "outside admitted retirement subtree"
                    );
                    grants.retain(|_, g| {
                        g.root
                            || !(g.prefix == prefix || g.prefix.starts_with(&format!("{prefix}/")))
                    });
                    Ok(serde_json::Value::Null)
                }
                Operation::Close => {
                    ensure!(!grant.root, "root cannot be closed by child protocol");
                    let prefix = grant.prefix.clone();
                    grants.retain(|_, g| {
                        g.root
                            || !(g.prefix == prefix || g.prefix.starts_with(&format!("{prefix}/")))
                    });
                    Ok(serde_json::Value::Null)
                }
                Operation::Finalize {
                    recipe,
                    failed,
                    outcome,
                } => {
                    ensure!(grant.root, "child cannot finalize root");
                    Ok(serde_json::to_value(
                        store.finalize_result(&recipe, &failed, outcome)?,
                    )?)
                }
                Operation::Report => {
                    ensure!(grant.root, "child cannot inspect sibling context");
                    Ok(serde_json::to_value(&store.tree.report)?)
                }
                Operation::Shutdown => {
                    ensure!(grant.root, "child cannot shut down writer");
                    shutdown = true;
                    Ok(serde_json::Value::Null)
                }
            }
        })();
        let response = match value {
            Ok(value) => Response { value, error: None },
            Err(_) => Response {
                value: serde_json::Value::Null,
                error: Some("rejected".into()),
            },
        };
        // A disconnected client may retry a published commit. The store verifies
        // idempotence rather than silently discarding a conflicting completion.
        if let Err(error) = write_frame(&mut stream, &super::private_fs::serialize(&response)?) {
            log::warn!("recovery acknowledgment failed: {error}");
        }
        if shutdown {
            break;
        }
    }
    Ok(())
}

thread_local! {static CURRENT:std::cell::RefCell<Option<Client>>=const {std::cell::RefCell::new(None)};}
pub struct Scope {
    previous: Option<Client>,
    active: Option<Client>,
    owner: Option<Client>,
}
pub fn enter(client: Option<Client>) -> Scope {
    enter_owned(client, None)
}
pub fn enter_owned(client: Option<Client>, owner: Option<Client>) -> Scope {
    let previous = CURRENT.with(|c| c.replace(client.clone()));
    Scope {
        previous,
        active: client,
        owner,
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|c| c.replace(self.previous.take()));
        if let Some(client) = &self.active
            && let Err(error) = match &self.owner {
                Some(owner) => owner.retire(client.prefix.clone()),
                None => client.close(),
            }
        {
            log::warn!("recovery attachment already retired or unavailable: {error}");
        }
    }
}
pub fn current_environment() -> Result<Option<String>> {
    CURRENT.with(|c| c.borrow().as_ref().map(Client::environment).transpose())
}
/// Used by the trusted usage-limit supervisor before waiting or unwinding.
pub fn notify(outcome: Outcome) -> Result<()> {
    if let Some(client) = Client::from_environment()? {
        client.outcome(outcome)?;
    }
    Ok(())
}
// Do not expose attachment values through Debug, diagnostics or public results.

fn connect(socket: &PathBuf) -> std::io::Result<UnixStream> {
    #[cfg(target_os = "linux")]
    if let Some(name) = socket.to_str().and_then(|s| s.strip_prefix('@')) {
        use std::os::linux::net::SocketAddrExt;
        return UnixStream::connect_addr(&std::os::unix::net::SocketAddr::from_abstract_name(
            name.as_bytes(),
        )?);
    }
    UnixStream::connect(socket)
}

/// A delegated process must not leave its subtree credential live after return.
pub struct InvocationGuard(Client);
impl Drop for InvocationGuard {
    fn drop(&mut self) {
        if let Err(error) = self.0.close() {
            log::warn!("delegated recovery attachment already retired or unavailable: {error}");
        }
    }
}
