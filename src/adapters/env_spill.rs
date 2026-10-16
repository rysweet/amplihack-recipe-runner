//! #130: per-value environment spill for bash steps.
//!
//! Linux refuses `execve` with `E2BIG` when any single `KEY=value\0` string is
//! longer than `MAX_ARG_STRLEN`, even when the whole block fits `ARG_MAX`.
//! Recipe values that are too large for one env string, plus the largest
//! remaining values needed to bring the total under the measured budget, are
//! written to private per-step files instead. A preamble prepended to the bash
//! script reads each file back into a shell variable of the same name, so the
//! script sees byte-identical values.
//!
//! Spilled values are shell-local (not exported): exporting a value this large
//! would make every external command the script runs fail with the same E2BIG.
//! Values are never logged; only names, sizes and (at debug) the directory.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;

/// Linux's per-string cap for argv/envp entries copied during `execve`.
pub(crate) const MAX_ARG_STRLEN: usize = 131_072;

/// Largest `KEY=value\0` string kept inline in the child environment.
///
/// The 4096-byte margin is conservative headroom below the kernel's per-string
/// limit, so that a value near the cap is spilled rather than kept inline at
/// the edge of `MAX_ARG_STRLEN`.
pub(crate) const PER_VALUE_ENV_CAP: usize = MAX_ARG_STRLEN - 4096;

/// Byte cost of one `KEY=value\0` environment string, as the kernel counts it.
pub(crate) fn env_pair_bytes(key: &str, value: &str) -> usize {
    key.len() + 1 + value.len() + 1
}

/// Names bash treats specially; assigning them with `read` would change shell
/// behaviour or fail outright.
const UNSPILLABLE_NAMES: &[&str] = &[
    "IFS",
    "PATH",
    "HOME",
    "EUID",
    "PPID",
    "SHELLOPTS",
    "UID",
    "GROUPS",
    "FUNCNAME",
    "LINENO",
    "RANDOM",
    "SRANDOM",
    "SECONDS",
    "EPOCHSECONDS",
    "EPOCHREALTIME",
    "HISTCMD",
    "DIRSTACK",
    "OPTIND",
    "OPTARG",
    "PS4",
    "ENV",
    "POSIXLY_CORRECT",
    "CDPATH",
    "GLOBIGNORE",
];

/// Whether `name` can safely be assigned with `read` in the step preamble.
pub(crate) fn is_spillable_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let valid_identifier = match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
            bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }
        _ => false,
    };
    valid_identifier
        && !UNSPILLABLE_NAMES.contains(&name)
        && !name.starts_with("BASH")
        && !name.starts_with("COMP_")
}

/// Which `extra_env` keys move to files and which are dropped outright.
pub(crate) struct SpillPlan {
    /// Keys whose values are written to spill files, in plan order.
    pub(crate) spill: Vec<String>,
    /// Over-cap keys that cannot be `read` safely; removed with a warning.
    pub(crate) drop: Vec<String>,
}

impl SpillPlan {
    fn is_empty(&self) -> bool {
        self.spill.is_empty() && self.drop.is_empty()
    }
}

/// Plan which recipe values leave the environment. Pure.
///
/// `merged` is the full child environment (inherited + `extra_env`); only keys
/// from `extra_env` are ever spilled or dropped.
///
/// 1. Cap phase (name order): every `extra_env` value whose env string exceeds
///    [`PER_VALUE_ENV_CAP`] is spilled, or dropped if its name can't be `read`.
/// 2. Budget phase: the largest remaining spillable values (ties by name) are
///    spilled until the whole merged env fits `budget`.
///
/// Empty values are never spilled; a key's value is taken from `merged`.
pub(crate) fn plan_env_spill(
    merged: &HashMap<String, String>,
    extra_env: &HashMap<String, String>,
    budget: usize,
) -> SpillPlan {
    let mut plan = SpillPlan {
        spill: Vec::new(),
        drop: Vec::new(),
    };

    let mut candidates: Vec<(&str, usize)> = extra_env
        .keys()
        .filter_map(|k| merged.get_key_value(k))
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.as_str(), env_pair_bytes(k, v)))
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(b.0));

    let mut total: usize = merged.iter().map(|(k, v)| env_pair_bytes(k, v)).sum();

    let mut remaining = Vec::with_capacity(candidates.len());
    for (name, bytes) in candidates {
        if bytes > PER_VALUE_ENV_CAP {
            total -= bytes;
            if is_spillable_name(name) {
                plan.spill.push(name.to_string());
            } else {
                plan.drop.push(name.to_string());
            }
        } else if is_spillable_name(name) {
            remaining.push((name, bytes));
        }
    }

    // Largest first; the stable sort keeps name order for equal sizes.
    remaining.sort_by_key(|&(_, bytes)| std::cmp::Reverse(bytes));
    for (name, bytes) in remaining {
        if total <= budget {
            break;
        }
        total -= bytes;
        plan.spill.push(name.to_string());
    }

    plan
}

/// Private per-step spill directory. Removed on drop.
///
/// Deliberately not `Debug`: nothing here may ever format value bytes.
pub(crate) struct EnvSpill {
    dir: tempfile::TempDir,
    entries: Vec<(String, PathBuf)>,
}

impl EnvSpill {
    /// Write each `(name, value)` to its own 0600 file in a fresh 0700
    /// directory under `root`.
    pub(crate) fn write(root: &Path, items: &[(&str, &str)]) -> anyhow::Result<Self> {
        let dir = private_tempdir(root)?;
        let mut entries = Vec::with_capacity(items.len());
        for (i, (name, value)) in items.iter().enumerate() {
            let path = dir.path().join(format!("v{i}"));
            write_private_file(&path, value.as_bytes())
                .with_context(|| format!("failed to spill {name} ({} bytes)", value.len()))?;
            entries.push((name.to_string(), path));
        }
        Ok(Self { dir, entries })
    }

    /// Shell text that loads every spilled value into its variable.
    ///
    /// One line per entry. `read -d ''` stops at end of file, so it returns
    /// non-zero after reading the whole value; `|| :` absorbs that. If the
    /// redirect itself fails the step exits 125 before the user command runs.
    pub(crate) fn preamble(&self) -> String {
        let mut out = String::new();
        for (name, path) in self.entries() {
            let path = path
                .to_str()
                .expect("private_tempdir rejects non-UTF-8 spill directories");
            out.push_str(&format!(
                "{{ IFS= read -r -d '' {name} || :; }} < {} || \
                 {{ printf 'amplihack: failed to load spilled env value {name}\\n' >&2; exit 125; }}\n",
                shell_single_quote(path)
            ));
        }
        out
    }

    /// The spill directory.
    pub(crate) fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// `(name, file)` pairs in write order.
    pub(crate) fn entries(&self) -> &[(String, PathBuf)] {
        &self.entries
    }
}

fn private_tempdir(root: &Path) -> anyhow::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("amplihack-env-spill-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let dir = builder
        .tempdir_in(root)
        .context("failed to create private env spill directory")?;
    if dir.path().to_str().is_none() {
        anyhow::bail!("env spill directory path is not valid UTF-8");
    }
    Ok(dir)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

/// Warning for an over-cap value that is dropped. Dropped names failed
/// [`is_spillable_name`], and can come from untrusted JSON keys flattened into
/// `RECIPE_VAR_*`, so they are escaped to keep control characters (ANSI/OSC
/// sequences, newlines) out of the operator's terminal and logs.
fn drop_warning(name: &str, len: usize) -> String {
    format!(
        "Dropping env var {} ({len} bytes) from bash step: too large for one \
         environment string and not a name the step preamble can assign",
        name.escape_debug()
    )
}

/// Wrap `s` in single quotes for bash, escaping embedded `'` as `'\''`.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Plan, write and strip spilled values from `child_env` for one bash step.
/// Returns `None` (and touches nothing) when no value needs to move.
pub(crate) fn spill_for_bash(
    child_env: &mut HashMap<String, String>,
    extra_env: &HashMap<String, String>,
    budget: usize,
) -> anyhow::Result<Option<EnvSpill>> {
    let plan = plan_env_spill(child_env, extra_env, budget);
    if plan.is_empty() {
        return Ok(None);
    }

    // `read -d ''` stops at the first NUL, so such a value can't round-trip.
    for name in &plan.spill {
        let value = &child_env[name];
        if value.contains('\0') {
            anyhow::bail!(
                "recipe variable {name} ({} bytes) contains a NUL byte and cannot be passed to a bash step",
                value.len()
            );
        }
    }

    for name in &plan.drop {
        if let Some(value) = child_env.remove(name) {
            log::warn!("{}", drop_warning(name, value.len()));
        }
    }

    let values: Vec<(String, String)> = plan
        .spill
        .iter()
        .filter_map(|name| child_env.remove(name).map(|v| (name.clone(), v)))
        .collect();
    if values.is_empty() {
        return Ok(None);
    }

    let items: Vec<(&str, &str)> = values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let spill = EnvSpill::write(&spill_root(), &items)?;

    let summary: Vec<String> = values
        .iter()
        .map(|(k, v)| format!("{k} ({} bytes)", v.len()))
        .collect();
    log::info!(
        "Spilled {} env value(s) for bash step to private files: {}",
        values.len(),
        summary.join(", ")
    );
    log::debug!("Env spill directory: {}", spill.dir().display());

    Ok(Some(spill))
}

#[cfg(test)]
thread_local! {
    static SPILL_ROOT_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Directory under which per-step spill directories are created.
pub(crate) fn spill_root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = SPILL_ROOT_OVERRIDE.with(|r| r.borrow().clone()) {
        return root;
    }
    std::env::temp_dir()
}

/// Test-only override for [`spill_root`] on the current thread.
#[cfg(test)]
pub(crate) struct SpillRootGuard {
    previous: Option<PathBuf>,
}

#[cfg(test)]
impl SpillRootGuard {
    pub(crate) fn set(root: &Path) -> Self {
        let previous = SPILL_ROOT_OVERRIDE.with(|r| r.replace(Some(root.to_path_buf())));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for SpillRootGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        SPILL_ROOT_OVERRIDE.with(|r| *r.borrow_mut() = previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn map_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn entries_in(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        v.sort();
        v
    }

    /// Run `script` under bash with an empty environment.
    fn run_bash(script: &str) -> std::process::Output {
        Command::new("/bin/bash")
            .arg("-c")
            .arg(script)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    }

    // ── sizes and constants ───────────────────────────────────────────

    #[test]
    fn per_value_cap_leaves_a_margin_under_max_arg_strlen() {
        assert_eq!(MAX_ARG_STRLEN, 131_072);
        assert_eq!(PER_VALUE_ENV_CAP, 126_976);
    }

    #[test]
    fn env_pair_bytes_counts_equals_sign_and_nul() {
        assert_eq!(env_pair_bytes("KEY", "value"), 3 + 1 + 5 + 1);
        assert_eq!(env_pair_bytes("A", ""), 3);
    }

    #[test]
    fn drop_warning_escapes_control_characters_in_the_name() {
        let w = drop_warning("RECIPE_VAR_x__evil\u{1b}]0;pwn\u{7}\nWARN fake", 9);
        assert!(!w.contains('\u{1b}') && !w.contains('\u{7}') && !w.contains('\n'));
        assert!(w.contains("RECIPE_VAR_x__evil"));
        assert!(w.contains("(9 bytes)"));
    }

    // ── S2: names ─────────────────────────────────────────────────────

    #[test]
    fn spillable_names_are_plain_identifiers() {
        for name in [
            "RECIPE_VAR_x",
            "RECIPE_VAR_build__log",
            "GEN",
            "_private",
            "a1",
            "TASK_DESCRIPTION",
        ] {
            assert!(is_spillable_name(name), "{name} must be spillable");
        }
    }

    #[test]
    fn unsafe_or_special_names_are_not_spillable() {
        for name in [
            "",
            "9X",
            "A-B",
            "A B",
            "É",
            "IFS",
            "PATH",
            "HOME",
            "BASHOPTS",
            "BASHPID",
            "BASH_ENV",
            "BASH_FOO",
            "SHELLOPTS",
            "EUID",
            "UID",
            "PPID",
            "GROUPS",
            "FUNCNAME",
            "LINENO",
            "RANDOM",
            "SRANDOM",
            "SECONDS",
            "HISTCMD",
            "DIRSTACK",
            "OPTIND",
            "OPTARG",
            "PS4",
            "ENV",
            "POSIXLY_CORRECT",
            "CDPATH",
            "GLOBIGNORE",
            "COMP_WORDS",
        ] {
            assert!(!is_spillable_name(name), "{name:?} must NOT be spillable");
        }
    }

    // ── T12: planner ──────────────────────────────────────────────────

    #[test]
    fn plan_is_empty_when_everything_fits() {
        let extra = map_of(&[("RECIPE_VAR_a", "small"), ("A", "small")]);
        let mut merged = extra.clone();
        merged.insert("PATH".into(), "/usr/bin".into());
        let plan = plan_env_spill(&merged, &extra, 1 << 20);
        assert!(plan.spill.is_empty());
        assert!(plan.drop.is_empty());
    }

    #[test]
    fn plan_spills_every_value_over_the_per_value_cap_even_under_budget() {
        let big = "x".repeat(130 * 1024);
        let extra = map_of(&[
            ("RECIPE_VAR_zeta", big.as_str()),
            ("RECIPE_VAR_alpha", big.as_str()),
            ("RECIPE_VAR_small", "s"),
        ]);
        let plan = plan_env_spill(&extra, &extra, usize::MAX);
        // Cap phase is ordered by name.
        assert_eq!(plan.spill, vec!["RECIPE_VAR_alpha", "RECIPE_VAR_zeta"]);
        assert!(plan.drop.is_empty());
    }

    #[test]
    fn plan_budget_phase_spills_largest_first_until_total_fits() {
        // Pair sizes: A=103, B=303, C=203, INH=55 → total 664.
        let extra = map_of(&[
            ("A", &"a".repeat(100)),
            ("B", &"b".repeat(300)),
            ("C", &"c".repeat(200)),
        ]);
        let mut merged = extra.clone();
        merged.insert("INH".into(), "i".repeat(50));

        assert!(plan_env_spill(&merged, &extra, 664).spill.is_empty());
        assert_eq!(plan_env_spill(&merged, &extra, 400).spill, vec!["B"]);
        assert_eq!(plan_env_spill(&merged, &extra, 361).spill, vec!["B"]);
        assert_eq!(plan_env_spill(&merged, &extra, 200).spill, vec!["B", "C"]);
        // Inherited values are never spilled, even when the budget can't be met.
        assert_eq!(
            plan_env_spill(&merged, &extra, 10).spill,
            vec!["B", "C", "A"]
        );
    }

    #[test]
    fn plan_budget_phase_breaks_ties_by_name_and_is_deterministic() {
        let v = "v".repeat(64);
        let extra = map_of(&[("E", v.as_str()), ("D", v.as_str()), ("F", v.as_str())]);
        let total: usize = extra.iter().map(|(k, v)| k.len() + v.len() + 2).sum();
        for _ in 0..20 {
            let plan = plan_env_spill(&extra, &extra, total - 1);
            assert_eq!(plan.spill, vec!["D"]);
        }
    }

    #[test]
    fn plan_cap_phase_precedes_budget_phase() {
        let big = "x".repeat(130 * 1024);
        let mid = "m".repeat(4096);
        let extra = map_of(&[
            ("RECIPE_VAR_big", big.as_str()),
            ("RECIPE_VAR_mid", mid.as_str()),
            ("RECIPE_VAR_tiny", "t"),
        ]);
        let plan = plan_env_spill(&extra, &extra, 1024);
        assert_eq!(plan.spill, vec!["RECIPE_VAR_big", "RECIPE_VAR_mid"]);
    }

    #[test]
    fn plan_never_spills_empty_values() {
        let extra = map_of(&[("RECIPE_VAR_empty", ""), ("EMPTY", "")]);
        let plan = plan_env_spill(&extra, &extra, 0);
        assert!(plan.spill.is_empty(), "{:?}", plan.spill);
    }

    #[test]
    fn plan_never_touches_inherited_vars() {
        let big = "x".repeat(200 * 1024);
        let extra = map_of(&[("RECIPE_VAR_a", "a")]);
        let mut merged = extra.clone();
        merged.insert("INHERITED_HUGE".into(), big);
        let plan = plan_env_spill(&merged, &extra, usize::MAX);
        assert!(!plan.spill.iter().any(|k| k == "INHERITED_HUGE"));
        assert!(!plan.drop.iter().any(|k| k == "INHERITED_HUGE"));
    }

    #[test]
    fn plan_handles_legacy_alias_independently() {
        // Well over the cap: both forms spill.
        let big = "x".repeat(130 * 1024);
        let extra = map_of(&[("RECIPE_VAR_x", big.as_str()), ("X", big.as_str())]);
        let plan = plan_env_spill(&extra, &extra, usize::MAX);
        assert_eq!(plan.spill, vec!["RECIPE_VAR_x", "X"]);

        // In the band right at the cap only the longer key crosses it.
        let v = "x".repeat(PER_VALUE_ENV_CAP - 13);
        assert!("RECIPE_VAR_x".len() + v.len() + 2 > PER_VALUE_ENV_CAP);
        assert!("X".len() + v.len() + 2 <= PER_VALUE_ENV_CAP);
        let extra = map_of(&[("RECIPE_VAR_x", v.as_str()), ("X", v.as_str())]);
        let plan = plan_env_spill(&extra, &extra, usize::MAX);
        assert_eq!(plan.spill, vec!["RECIPE_VAR_x"]);
    }

    #[test]
    fn plan_drops_over_cap_names_that_cannot_be_read() {
        let big = "x".repeat(130 * 1024);
        let extra = map_of(&[("BASH_FOO", big.as_str()), ("RECIPE_VAR_ok", big.as_str())]);
        let plan = plan_env_spill(&extra, &extra, usize::MAX);
        assert_eq!(plan.spill, vec!["RECIPE_VAR_ok"]);
        assert_eq!(plan.drop, vec!["BASH_FOO"]);
    }

    #[test]
    fn plan_leaves_small_unreadable_names_to_bounded_env() {
        let extra = map_of(&[("BASH_FOO", &"x".repeat(1024))]);
        let plan = plan_env_spill(&extra, &extra, 16);
        assert!(plan.spill.is_empty());
        assert!(plan.drop.is_empty());
    }

    // ── T5: spill files ───────────────────────────────────────────────

    #[test]
    fn spill_files_are_private_and_named_by_index() {
        let root = tempfile::tempdir().unwrap();
        let spill =
            EnvSpill::write(root.path(), &[("RECIPE_VAR_a", "alpha\n\n"), ("B", "beta")]).unwrap();

        let dir = spill.dir().to_path_buf();
        assert_eq!(dir.parent().unwrap(), root.path());
        assert!(
            dir.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("amplihack-env-spill-")
        );
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "spill dir must be 0700, got {dir_mode:o}");

        let names: Vec<&str> = spill.entries().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["RECIPE_VAR_a", "B"]);
        for (i, (_, path)) in spill.entries().iter().enumerate() {
            assert_eq!(path.parent().unwrap(), dir);
            assert_eq!(path.file_name().unwrap().to_str().unwrap(), format!("v{i}"));
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "spill file must be 0600, got {mode:o}");
        }
        assert_eq!(std::fs::read(&spill.entries()[0].1).unwrap(), b"alpha\n\n");
        assert_eq!(std::fs::read(&spill.entries()[1].1).unwrap(), b"beta");
    }

    #[test]
    fn dropping_the_spill_removes_its_directory() {
        let root = tempfile::tempdir().unwrap();
        let spill = EnvSpill::write(root.path(), &[("A", "a")]).unwrap();
        let dir = spill.dir().to_path_buf();
        assert!(dir.exists());
        drop(spill);
        assert!(!dir.exists());
        assert!(entries_in(root.path()).is_empty());
    }

    // ── preamble ──────────────────────────────────────────────────────

    #[test]
    fn preamble_round_trips_values_byte_for_byte() {
        let root = tempfile::tempdir().unwrap();
        let tricky = "  lead\nembedded\\ 'single' \"double\" $(echo no) `echo no` * ; trail  \n\n";
        let spill =
            EnvSpill::write(root.path(), &[("RECIPE_VAR_t", tricky), ("T", tricky)]).unwrap();
        let script = format!(
            "{}printf %s \"$RECIPE_VAR_t\" | od -An -v -tx1 | tr -d ' \\n'; echo; \
             printf %s \"$T\" | od -An -v -tx1 | tr -d ' \\n'",
            spill.preamble()
        );
        let out = run_bash(&script);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let expected: String = tricky.bytes().map(|b| format!("{b:02x}")).collect();
        let stdout = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines, vec![expected.as_str(), expected.as_str()]);
    }

    #[test]
    fn preamble_contains_names_and_paths_but_never_values() {
        let root = tempfile::tempdir().unwrap();
        let sentinel = "SENTINEL_VALUE_must_not_appear_in_shell_text";
        let spill = EnvSpill::write(root.path(), &[("RECIPE_VAR_s", sentinel)]).unwrap();
        let pre = spill.preamble();
        assert!(!pre.contains(sentinel));
        assert!(pre.contains("RECIPE_VAR_s"));
        assert!(pre.contains("read -r -d ''"));
        assert!(pre.ends_with('\n'));
        assert_eq!(
            pre.lines().count(),
            1,
            "one preamble line per spilled value"
        );
    }

    #[test]
    fn preamble_quotes_awkward_spill_root_paths() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("it's a \"dir\" $(x)\nnext");
        std::fs::create_dir(&root).unwrap();
        let spill = EnvSpill::write(&root, &[("V", "ok")]).unwrap();
        let out = run_bash(&format!("{}printf %s \"$V\"", spill.preamble()));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, b"ok");
    }

    #[test]
    fn spilled_values_are_shell_local_not_exported() {
        let root = tempfile::tempdir().unwrap();
        let spill = EnvSpill::write(root.path(), &[("RECIPE_VAR_big", "v")]).unwrap();
        let out = run_bash(&format!(
            "{}env | grep -c '^RECIPE_VAR_big=' || true",
            spill.preamble()
        ));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0");
    }

    // ── S3: fail closed ───────────────────────────────────────────────

    #[test]
    fn missing_spill_file_fails_closed_before_the_user_command() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("ran");
        let spill = EnvSpill::write(root.path(), &[("RECIPE_VAR_gone", "v")]).unwrap();
        std::fs::remove_file(&spill.entries()[0].1).unwrap();
        let out = run_bash(&format!("{}touch '{}'", spill.preamble(), marker.display()));
        assert_eq!(out.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&out.stderr).contains("RECIPE_VAR_gone"));
        assert!(
            !marker.exists(),
            "user command must not run after a failed load"
        );
    }

    // ── spill_for_bash ────────────────────────────────────────────────

    #[test]
    fn spill_for_bash_is_a_no_op_when_nothing_moves() {
        // T7
        let root = tempfile::tempdir().unwrap();
        let _g = SpillRootGuard::set(root.path());
        let extra = map_of(&[("RECIPE_VAR_a", "a"), ("A", "a")]);
        let mut child = extra.clone();
        child.insert("PATH".into(), "/usr/bin".into());
        let before = child.clone();
        let spill = spill_for_bash(&mut child, &extra, 1 << 20).unwrap();
        assert!(spill.is_none());
        assert_eq!(child, before);
        assert!(
            entries_in(root.path()).is_empty(),
            "no spill dir may be created"
        );
    }

    #[test]
    fn spill_for_bash_removes_spilled_and_dropped_keys_from_the_env() {
        // S5/SR-6
        let root = tempfile::tempdir().unwrap();
        let _g = SpillRootGuard::set(root.path());
        let big = "x".repeat(130 * 1024);
        let extra = map_of(&[
            ("RECIPE_VAR_big", big.as_str()),
            ("BASH_FOO", big.as_str()),
            ("RECIPE_VAR_small", "s"),
        ]);
        let mut child = extra.clone();
        child.insert("PATH".into(), "/usr/bin".into());
        let spill = spill_for_bash(&mut child, &extra, 1 << 20)
            .unwrap()
            .expect("an over-cap value must spill");
        assert!(!child.contains_key("RECIPE_VAR_big"));
        assert!(!child.contains_key("BASH_FOO"));
        assert_eq!(child.get("RECIPE_VAR_small").map(String::as_str), Some("s"));
        assert_eq!(child.get("PATH").map(String::as_str), Some("/usr/bin"));
        let names: Vec<&str> = spill.entries().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["RECIPE_VAR_big"]);
        assert_eq!(spill.dir().parent().unwrap(), root.path());
    }

    #[test]
    fn spill_for_bash_rejects_nul_without_leaking_the_value() {
        // S4/S7
        let root = tempfile::tempdir().unwrap();
        let _g = SpillRootGuard::set(root.path());
        let mut value = "SENTINEL_SECRET_".repeat(9000);
        value.push('\0');
        let extra = map_of(&[("RECIPE_VAR_nul", value.as_str())]);
        let mut child = extra.clone();
        let err = match spill_for_bash(&mut child, &extra, 1 << 20) {
            Ok(_) => panic!("a NUL byte cannot survive read -d '' and must be rejected"),
            Err(e) => format!("{e:#} {e:?}"),
        };
        assert!(err.contains("RECIPE_VAR_nul"), "{err}");
        assert!(
            !err.contains("SENTINEL_SECRET_"),
            "error must not contain value bytes"
        );
        assert!(entries_in(root.path()).is_empty());
    }

    #[test]
    fn spill_root_defaults_to_temp_dir_and_honours_the_test_override() {
        assert_eq!(spill_root(), std::env::temp_dir());
        let root = tempfile::tempdir().unwrap();
        {
            let _g = SpillRootGuard::set(root.path());
            assert_eq!(spill_root(), root.path());
        }
        assert_eq!(spill_root(), std::env::temp_dir());
    }
}
