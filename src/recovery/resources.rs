//! Resource observations and copies; declarations never authorize deletion.
use super::{model::*, private_fs::Directory};
use anyhow::{Result, ensure};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::File,
    io::Read,
    os::{fd::FromRawFd, unix::ffi::OsStrExt},
    path::{Component, Path},
    process::Command,
};
const MAX_RESOURCE: u64 = 16 * 1024 * 1024;

/// Read an ordinary external file without following any component's symlink.
pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    ensure!(path.is_absolute(), "resource must be absolute");
    let parts: Vec<_> = path.components().collect();
    let root = CString::new("/")?;
    // SAFETY: valid NUL-terminated root path.
    let mut fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    ensure!(fd >= 0, "cannot open resource root");
    // SAFETY: fd is newly owned.
    let mut current = unsafe { File::from_raw_fd(fd) };
    use std::os::fd::AsRawFd;
    for (i, part) in parts.iter().enumerate() {
        if *part == Component::RootDir {
            continue;
        }
        let Component::Normal(name) = part else {
            anyhow::bail!("non-normal resource path")
        };
        let n = CString::new(name.as_bytes())?;
        // SAFETY: valid held directory and relative component; no symlinks.
        fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                n.as_ptr(),
                libc::O_RDONLY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK
                    | if i + 1 == parts.len() {
                        0
                    } else {
                        libc::O_DIRECTORY
                    },
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: openat returned a new descriptor.
        current = unsafe { File::from_raw_fd(fd) };
    }
    let before = current.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_RESOURCE,
        "unavailable or oversized resource"
    );
    let mut bytes = vec![];
    current
        .by_ref()
        .take(MAX_RESOURCE + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RESOURCE
            && current.metadata()?.modified()? == before.modified()?,
        "resource changed during observation"
    );
    Ok(bytes)
}
pub fn git(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    // Git hooks export repository selectors that outrank -C. Observations must
    // describe the declared checkout, never the hook caller's repository.
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|k| k.starts_with("GIT_")) {
            command.env_remove(key);
        }
    }
    let out = command
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    ensure!(out.status.success(), "repository observation unavailable");
    ensure!(
        out.stdout.len() as u64 <= MAX_RESOURCE,
        "oversized repository observation"
    );
    Ok(out.stdout)
}
pub fn repository(path: &Path) -> Result<String> {
    let head = git(path, &["rev-parse", "--verify", "HEAD"])?;
    let branch = git(path, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    let files = git(
        path,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut state = BTreeMap::new();
    let mut total = 0;
    let checkout = path.canonicalize()?;
    for raw in files.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let name = std::str::from_utf8(raw)?;
        let relative = Path::new(name);
        ensure!(
            relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
            "unsafe repository file"
        );
        let full = path.join(relative);
        if std::fs::symlink_metadata(&full).is_ok_and(|m| m.file_type().is_symlink()) {
            // Observe link text, never follow it to read arbitrary external data.
            // Targets inside the checkout are covered by the tracked-file scan.
            ensure!(
                full.canonicalize()?.starts_with(&checkout),
                "external repository symlink dependency"
            );
            let link = std::fs::read_link(&full)?;
            state.insert(
                name.to_owned(),
                format!("link:{}", digest(link.as_os_str().as_bytes())),
            );
            continue;
        }
        let value = match read_file(&full) {
            Ok(b) => {
                total += b.len();
                ensure!(
                    total <= 64 * 1024 * 1024,
                    "repository observation too large"
                );
                digest(&b)
            }
            Err(_) if !full.exists() => "missing".into(),
            Err(e) => return Err(e),
        };
        state.insert(name.to_owned(), value);
    }
    fingerprint(&(
        head,
        branch,
        git(path, &["diff", "--cached", "--raw"])?,
        state,
    ))
}
pub fn observe(
    resources: &[Resource],
    ctx: &crate::context::RecipeContext,
    working_dir: &str,
) -> Vec<Observation> {
    let base = Path::new(working_dir);
    let base = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(base)
    };
    let mut result = vec![];
    // The active checkout is always a dependency when Git identifies it.
    if git(&base, &["rev-parse", "--git-common-dir"]).is_ok() {
        result.push(Observation {
            kind: "repository".into(),
            path: base.display().to_string(),
            digest: repository(&base).unwrap_or_default(),
            context_key: None,
            blob: None,
        });
    }
    for resource in resources {
        let (kind, raw, key) = match resource {
            Resource::File { path } => ("file", path, None),
            Resource::Repository { path } => ("repository", path, None),
            Resource::Artifact { path, context_key } => {
                ("artifact", path, Some(context_key.clone()))
            }
            Resource::Live { name } => {
                result.push(Observation {
                    kind: "live".into(),
                    path: name.clone(),
                    digest: String::new(),
                    context_key: None,
                    blob: None,
                });
                continue;
            }
        };
        let rendered = ctx.render(raw);
        let path = Path::new(&rendered);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        };
        let observed = if kind == "repository" {
            repository(&path)
        } else {
            read_file(&path).map(|b| digest(&b))
        };
        result.push(Observation {
            kind: kind.into(),
            path: path.display().to_string(),
            digest: observed.unwrap_or_default(),
            context_key: key,
            blob: None,
        });
    }
    result
}
pub fn capture(resources: &mut [Observation], directory: &Directory) -> Result<()> {
    let artifacts = directory.child("artifacts", true)?;
    for resource in resources {
        if resource.kind == "artifact" {
            let bytes = read_file(Path::new(&resource.path))?;
            let hash = digest(&bytes);
            ensure!(
                resource.digest == hash,
                "artifact changed before completion publication"
            );
            artifacts.publish(&hash, &bytes)?;
            resource.blob = Some(hash);
        }
    }
    Ok(())
}
pub fn import(tree: &Tree, source: &Directory, target: &Directory) -> Result<()> {
    for record in &tree.records {
        for resource in &record.probe.resources {
            if let Some(blob) = &resource.blob {
                ensure!(
                    blob.len() == 64 && blob.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid artifact identifier"
                );
                let bytes = source.child("artifacts", false)?.read(blob)?;
                ensure!(
                    digest(&bytes) == resource.digest,
                    "artifact digest mismatch"
                );
                target.child("artifacts", true)?.publish(blob, &bytes)?;
            }
        }
    }
    Ok(())
}
