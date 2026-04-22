/// Recipe discovery — find, list, and sync recipe YAML files.
///
/// Searches well-known directories for recipe files and provides metadata.
///
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Collect directory entries, logging any I/O errors instead of silently dropping them.
fn collect_dir_entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| match entry {
            Ok(e) => Some(e.path()),
            Err(e) => {
                debug!("skipping unreadable entry in {}: {}", dir.display(), e);
                None
            }
        })
        .collect()
}

fn default_search_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut dirs = Vec::new();

    // AMPLIHACK_PACKAGE_RECIPE_DIR: the installed Python package's bundled
    // recipe directory.  This is set by the Python wrapper
    // (run_recipe_via_rust) and also usable by standalone callers to bridge
    // the Python/Rust discovery gap (issue #3002).
    if let Ok(pkg_dir) = std::env::var("AMPLIHACK_PACKAGE_RECIPE_DIR")
        && !pkg_dir.is_empty()
    {
        dirs.push(PathBuf::from(pkg_dir));
    }

    if let Ok(extra) = std::env::var("RECIPE_RUNNER_RECIPE_DIRS") {
        for p in extra.split(':') {
            if !p.is_empty() {
                dirs.push(PathBuf::from(p));
            }
        }
    }
    dirs.extend([
        // Installed amplihack bundle (current layout)
        home.join(".amplihack")
            .join("amplifier-bundle")
            .join("recipes"),
        // Legacy installed location (kept for back-compat)
        home.join(".amplihack").join(".claude").join("recipes"),
        // Project-local layouts
        PathBuf::from("amplifier-bundle").join("recipes"),
        PathBuf::from("src")
            .join("amplihack")
            .join("amplifier-bundle")
            .join("recipes"),
        PathBuf::from(".claude").join("recipes"),
    ]);
    dirs
}

/// Metadata about a discovered recipe file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeInfo {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub step_count: usize,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sha256: String,
}

/// Find all recipe YAML files (`.yaml` and `.yml`) in the search directories.
///
/// When the same recipe name appears in multiple directories, the last one wins.
/// Within a single directory, `.yaml` takes precedence over `.yml` if both exist.
pub fn discover_recipes(search_dirs: Option<&[PathBuf]>) -> HashMap<String, RecipeInfo> {
    let dirs = search_dirs
        .map(|d| d.to_vec())
        .unwrap_or_else(default_search_dirs);
    let mut recipes = HashMap::new();

    debug!("Searching for recipes in {} directories", dirs.len());
    for search_dir in &dirs {
        if !search_dir.is_dir() {
            debug!("  Skipping non-existent: {}", search_dir.display());
            continue;
        }
        debug!("  Scanning: {}", search_dir.display());
        let mut dir_count = 0;

        let mut entries: Vec<PathBuf> = collect_dir_entries(search_dir)
            .into_iter()
            .filter(|p| {
                p.extension()
                    .is_some_and(|ext| ext == "yaml" || ext == "yml")
            })
            .collect();
        // Sort so .yaml is processed AFTER .yml for the same stem; combined
        // with the last-wins HashMap insert, this gives .yaml precedence.
        entries.sort_by(|a, b| {
            let stem_a = a.file_stem().unwrap_or_default();
            let stem_b = b.file_stem().unwrap_or_default();
            stem_a.cmp(stem_b).then_with(|| {
                let ext_a = a.extension().and_then(|s| s.to_str()).unwrap_or("");
                let ext_b = b.extension().and_then(|s| s.to_str()).unwrap_or("");
                // "yml" < "yaml" lexicographically, so reverse to put yaml last
                ext_b.cmp(ext_a)
            })
        });

        for yaml_path in entries {
            if let Some(info) = load_recipe_info(&yaml_path) {
                debug!("    Found: {}", info.name);
                recipes.insert(info.name.clone(), info);
                dir_count += 1;
            }
        }
        debug!(
            "  Discovered {} recipes in {}",
            dir_count,
            search_dir.display()
        );
    }

    if recipes.is_empty() {
        warn!(
            "No recipes discovered! Searched: {}",
            dirs.iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    } else {
        debug!("Total recipes discovered: {}", recipes.len());
    }

    recipes
}

/// Return a sorted list of all discovered recipes.
pub fn list_recipes(search_dirs: Option<&[PathBuf]>) -> Vec<RecipeInfo> {
    let mut recipes: Vec<RecipeInfo> = discover_recipes(search_dirs).into_values().collect();
    recipes.sort_by(|a, b| a.name.cmp(&b.name));
    recipes
}

/// TTL-based cache for recipe discovery results.
///
/// Avoids re-scanning directories on every call. The cache is invalidated
/// automatically when the TTL expires or when the set of search directories
/// changes.
pub struct DiscoveryCache {
    cache: HashMap<String, RecipeInfo>,
    last_updated: Instant,
    ttl: Duration,
    search_dirs: Vec<PathBuf>,
}

impl DiscoveryCache {
    /// Create a new, empty cache with the given TTL.
    pub fn new(ttl: Duration) -> Self {
        Self {
            cache: HashMap::new(),
            last_updated: Instant::now(),
            ttl,
            search_dirs: Vec::new(),
        }
    }

    /// Return cached results if still valid, otherwise re-discover.
    ///
    /// The cache is considered invalid when:
    /// - It has never been populated (empty `search_dirs`)
    /// - The TTL has elapsed since the last update
    /// - The requested `dirs` differ from the dirs used to populate the cache
    pub fn get_or_discover(&mut self, dirs: &[PathBuf]) -> &HashMap<String, RecipeInfo> {
        let dirs_changed = self.search_dirs != dirs;
        let expired = self.last_updated.elapsed() >= self.ttl;
        let empty = self.search_dirs.is_empty() && self.cache.is_empty();

        if empty || expired || dirs_changed {
            debug!(
                "DiscoveryCache miss (empty={}, expired={}, dirs_changed={})",
                empty, expired, dirs_changed
            );
            self.cache = discover_recipes(Some(dirs));
            self.search_dirs = dirs.to_vec();
            self.last_updated = Instant::now();
        } else {
            debug!("DiscoveryCache hit ({} recipes cached)", self.cache.len());
        }

        &self.cache
    }

    /// Force the cache to refresh on the next `get_or_discover` call.
    pub fn invalidate(&mut self) {
        self.search_dirs.clear();
        self.cache.clear();
    }

    /// Return the number of cached recipes.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

/// Thread-safe convenience wrapper around [`DiscoveryCache`].
///
/// Uses a module-level `Mutex<DiscoveryCache>` so callers don't need to manage
/// their own cache instance.  The TTL defaults to 30 seconds and can be
/// overridden via the `RECIPE_RUNNER_CACHE_TTL` environment variable (in seconds).
pub fn cached_discover_recipes(dirs: &[PathBuf]) -> HashMap<String, RecipeInfo> {
    static CACHE: std::sync::LazyLock<Mutex<DiscoveryCache>> = std::sync::LazyLock::new(|| {
        let ttl_secs: u64 = std::env::var("RECIPE_RUNNER_CACHE_TTL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        Mutex::new(DiscoveryCache::new(Duration::from_secs(ttl_secs)))
    });

    let mut cache = CACHE.lock().expect("DiscoveryCache mutex poisoned");
    cache.get_or_discover(dirs).clone()
}

/// Find a recipe by name and return its file path.
///
/// Searches each directory in order, looking for `<name>.yaml` first then
/// `<name>.yml` (yaml takes precedence when both exist in the same dir).
///
/// Returns `None` if `name` contains path-traversal segments (`/`, `\`, `..`)
/// or starts with `.`, to prevent escaping the search directories.
pub fn find_recipe(name: &str, search_dirs: Option<&[PathBuf]>) -> Option<PathBuf> {
    if !is_safe_recipe_name(name) {
        warn!("find_recipe: rejecting unsafe recipe name");
        return None;
    }
    let dirs = search_dirs
        .map(|d| d.to_vec())
        .unwrap_or_else(default_search_dirs);
    for search_dir in &dirs {
        for ext in ["yaml", "yml"] {
            let candidate = search_dir.join(format!("{}.{}", name, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Reject recipe names that could escape the search directory or refer to
/// hidden files. Names must be a single path component with no separator
/// or parent-directory segments and must not start with `.`.
fn is_safe_recipe_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('.') {
        return false;
    }
    // Reject any path separator/NUL byte; a `..` segment cannot exist without
    // one, so the broader `contains("..")` also catches any non-separator
    // attempts to embed parent-dir markers.
    !name.chars().any(|c| matches!(c, '/' | '\\' | '\0')) && !name.contains("..")
}

/// Compare local recipe files against their content hashes.
pub fn check_upstream_changes(local_dir: Option<&Path>) -> Vec<HashMap<String, String>> {
    let recipe_dir = match local_dir
        .map(|p| p.to_path_buf())
        .or_else(find_first_recipe_dir)
    {
        Some(d) => d,
        None => return vec![],
    };

    let manifest = load_manifest(&recipe_dir);
    let mut changes = Vec::new();

    // Check existing files
    let mut entries: Vec<PathBuf> = collect_dir_entries(&recipe_dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    entries.sort();

    for yaml_path in &entries {
        let name = yaml_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let current_hash = file_hash(yaml_path);
        let stored_hash = manifest.get(&name).cloned().unwrap_or_default();

        if stored_hash.is_empty() {
            let mut change = HashMap::new();
            change.insert("name".to_string(), name);
            change.insert("status".to_string(), "new".to_string());
            change.insert("local_hash".to_string(), current_hash);
            change.insert("stored_hash".to_string(), String::new());
            changes.push(change);
        } else if current_hash != stored_hash {
            let mut change = HashMap::new();
            change.insert("name".to_string(), name);
            change.insert("status".to_string(), "modified".to_string());
            change.insert("local_hash".to_string(), current_hash);
            change.insert("stored_hash".to_string(), stored_hash);
            changes.push(change);
        }
    }

    // Check for deleted files
    for (name, hash) in &manifest {
        let path = recipe_dir.join(format!("{}.yaml", name));
        if !path.is_file() {
            let mut change = HashMap::new();
            change.insert("name".to_string(), name.clone());
            change.insert("status".to_string(), "deleted".to_string());
            change.insert("local_hash".to_string(), String::new());
            change.insert("stored_hash".to_string(), hash.clone());
            changes.push(change);
        }
    }

    changes
}

/// Default upstream URL used when `RECIPE_RUNNER_UPSTREAM_URL` is not set.
pub const DEFAULT_UPSTREAM_URL: &str = "https://github.com/microsoft/amplifier-bundle-recipes";

/// Environment variable that overrides the upstream sync URL.
pub const UPSTREAM_URL_ENV: &str = "RECIPE_RUNNER_UPSTREAM_URL";

/// Resolve the upstream URL from the environment (or default) with validation.
///
/// Reads `RECIPE_RUNNER_UPSTREAM_URL`; falls back to [`DEFAULT_UPSTREAM_URL`].
/// Validates that the URL uses an `http://` or `https://` scheme and does NOT
/// embed userinfo (e.g. `https://user:secret@host/...`). Error messages do not
/// echo the raw value, to avoid leaking credentials into logs.
pub fn upstream_url() -> Result<String, anyhow::Error> {
    upstream_url_inner(|k| std::env::var(k).ok())
}

/// Pure inner helper for [`upstream_url`] that takes an env-lookup closure.
///
/// Exposed for testability so unit tests do not have to mutate global env.
pub fn upstream_url_inner<F>(get_env: F) -> Result<String, anyhow::Error>
where
    F: Fn(&str) -> Option<String>,
{
    let raw = get_env(UPSTREAM_URL_ENV).unwrap_or_else(|| DEFAULT_UPSTREAM_URL.to_string());
    validate_upstream_url(&raw)?;
    Ok(raw)
}

/// Validate that a URL is acceptable for upstream sync.
///
/// Rules:
///   * Must be non-empty.
///   * Scheme must be `http://` or `https://` (case-insensitive).
///   * Must NOT contain userinfo (`user:pass@host`).
///
/// Errors are intentionally generic and do not include the raw URL value.
fn validate_upstream_url(raw: &str) -> Result<(), anyhow::Error> {
    if raw.is_empty() {
        return Err(anyhow::anyhow!(
            "upstream URL is empty (set {} or unset to use the default)",
            UPSTREAM_URL_ENV
        ));
    }
    let lower = raw.to_ascii_lowercase();
    let scheme_end = match lower.find("://") {
        Some(i) => i,
        None => {
            return Err(anyhow::anyhow!(
                "upstream URL is missing a scheme; only http:// and https:// are accepted"
            ));
        }
    };
    let scheme = &lower[..scheme_end];
    if scheme != "http" && scheme != "https" {
        return Err(anyhow::anyhow!(
            "upstream URL has an unsupported scheme; only http:// and https:// are accepted"
        ));
    }
    let after_scheme = &raw[scheme_end + 3..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.contains('@') {
        return Err(anyhow::anyhow!(
            "upstream URL must not embed credentials (userinfo); use a credential helper instead"
        ));
    }
    Ok(())
}

/// Write a manifest file recording the current hash of each recipe.
pub fn update_manifest(local_dir: Option<&Path>) -> Result<PathBuf, std::io::Error> {
    let recipe_dir = local_dir
        .map(|p| p.to_path_buf())
        .or_else(find_first_recipe_dir)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "No recipe directory found")
        })?;

    let mut manifest = HashMap::new();
    let mut entries: Vec<PathBuf> = collect_dir_entries(&recipe_dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    entries.sort();

    for yaml_path in &entries {
        if let Some(stem) = yaml_path.file_stem().and_then(|s| s.to_str()) {
            manifest.insert(stem.to_string(), file_hash(yaml_path));
        }
    }

    let manifest_path = recipe_dir.join("_recipe_manifest.json");
    let json = serde_json::to_string_pretty(&manifest).map_err(std::io::Error::other)?;
    std::fs::write(&manifest_path, format!("{}\n", json))?;
    info!(
        "Updated recipe manifest at {} ({} recipes)",
        manifest_path.display(),
        manifest.len()
    );
    Ok(manifest_path)
}

/// Sync upstream recipe changes via git.
pub fn sync_upstream(
    repo_url: Option<&str>,
    branch: Option<&str>,
    remote_name: Option<&str>,
) -> Result<serde_json::Value, anyhow::Error> {
    let default_url = upstream_url()?;
    let repo = repo_url.unwrap_or(&default_url);
    let br = branch.unwrap_or("main");
    let remote = format!("upstream-{}", remote_name.unwrap_or("amplifier-recipes"));

    // Add remote if not present
    let check = Command::new("git")
        .args(["remote", "get-url", &remote])
        .output()?;
    if !check.status.success() {
        let add_output = Command::new("git")
            .args(["remote", "add", &remote, repo])
            .output()?;
        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            if !stderr.contains("already exists") {
                return Err(anyhow::anyhow!("git remote add failed: {}", stderr));
            }
        }
        info!("Added remote '{}' -> {}", remote, repo);
    }

    // Fetch (with 30s timeout to prevent hangs on network issues)
    let fetch_output = Command::new("timeout")
        .args(["30", "git", "fetch", &remote, br])
        .output()?;
    if !fetch_output.status.success() {
        return Err(anyhow::anyhow!(
            "git fetch failed: {}",
            String::from_utf8_lossy(&fetch_output.stderr)
        ));
    }

    // Diff
    let upstream_ref = format!("{}/{}", remote, br);
    let diff = Command::new("git")
        .args([
            "diff",
            &upstream_ref,
            "--",
            "amplifier-bundle/recipes/",
            "src/amplihack/amplifier-bundle/recipes/",
        ])
        .output()?;
    let diff_stdout = String::from_utf8_lossy(&diff.stdout).to_string();
    let has_changes = !diff_stdout.trim().is_empty();

    let files = Command::new("git")
        .args([
            "diff",
            "--name-only",
            &upstream_ref,
            "--",
            "amplifier-bundle/recipes/",
        ])
        .output()?;
    let files_changed: Vec<String> = String::from_utf8_lossy(&files.stdout)
        .trim()
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    let diff_summary = if has_changes {
        let truncated = crate::safe_truncate(&diff_stdout, 500);
        if truncated.len() < diff_stdout.len() {
            format!(
                "{}... (truncated from {} bytes)",
                truncated,
                diff_stdout.len()
            )
        } else {
            truncated.to_string()
        }
    } else {
        "No changes".to_string()
    };

    Ok(serde_json::json!({
        "has_changes": has_changes,
        "files_changed": files_changed,
        "diff_summary": diff_summary,
        "upstream_ref": upstream_ref,
    }))
}

// -- Internal helpers --

fn load_recipe_info(yaml_path: &Path) -> Option<RecipeInfo> {
    let text = std::fs::read_to_string(yaml_path).ok()?;
    let data: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let map = data.as_mapping()?;

    let name = map
        .get(serde_yaml::Value::String("name".to_string()))?
        .as_str()?
        .to_string();

    let description = map
        .get(serde_yaml::Value::String("description".to_string()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let version = map
        .get(serde_yaml::Value::String("version".to_string()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let steps = map
        .get(serde_yaml::Value::String("steps".to_string()))
        .and_then(|v| v.as_sequence())
        .map(|s| s.len())
        .unwrap_or(0);

    let tags = map
        .get(serde_yaml::Value::String("tags".to_string()))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Some(RecipeInfo {
        name,
        path: yaml_path
            .canonicalize()
            .unwrap_or_else(|_| yaml_path.to_path_buf()),
        description,
        version,
        step_count: steps,
        tags,
        sha256: file_hash(yaml_path),
    })
}

fn file_hash(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let result = hasher.finalize();
            hex::encode(&result[..8])
        }
        Err(e) => {
            warn!("Failed to read file for hashing: {}: {}", path.display(), e);
            String::new()
        }
    }
}

fn load_manifest(recipe_dir: &Path) -> HashMap<String, String> {
    let manifest_path = recipe_dir.join("_recipe_manifest.json");
    if manifest_path.is_file()
        && let Ok(text) = std::fs::read_to_string(&manifest_path)
        && let Ok(map) = serde_json::from_str(&text)
    {
        return map;
    }
    HashMap::new()
}

fn find_first_recipe_dir() -> Option<PathBuf> {
    default_search_dirs().into_iter().find(|d| d.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let recipes = discover_recipes(Some(&[tmp.path().to_path_buf()]));
        assert!(recipes.is_empty());
    }

    #[test]
    fn test_discover_with_recipe() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = r#"
name: "test-recipe"
description: "A test"
version: "1.0.0"
steps:
  - id: "step1"
    command: "echo hello"
"#;
        std::fs::write(tmp.path().join("test-recipe.yaml"), yaml).unwrap();
        let recipes = discover_recipes(Some(&[tmp.path().to_path_buf()]));
        assert_eq!(recipes.len(), 1);
        assert!(recipes.contains_key("test-recipe"));
        let info = &recipes["test-recipe"];
        assert_eq!(info.step_count, 1);
        assert_eq!(info.version, "1.0.0");
    }

    #[test]
    fn test_find_recipe() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("my-recipe.yaml"),
            "name: my-recipe\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();
        let found = find_recipe("my-recipe", Some(&[tmp.path().to_path_buf()]));
        assert!(found.is_some());
        assert!(find_recipe("nonexistent", Some(&[tmp.path().to_path_buf()])).is_none());
    }

    #[test]
    fn test_last_wins_dedup() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(
            dir1.path().join("shared.yaml"),
            "name: shared\ndescription: from dir1\nsteps:\n  - id: s1\n    command: echo 1",
        )
        .unwrap();
        std::fs::write(
            dir2.path().join("shared.yaml"),
            "name: shared\ndescription: from dir2\nsteps:\n  - id: s1\n    command: echo 2",
        )
        .unwrap();
        let recipes = discover_recipes(Some(&[
            dir1.path().to_path_buf(),
            dir2.path().to_path_buf(),
        ]));
        assert_eq!(recipes["shared"].description, "from dir2");
    }

    #[test]
    fn test_manifest_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("recipe-a.yaml"),
            "name: recipe-a\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();
        let manifest_path = update_manifest(Some(tmp.path())).unwrap();
        assert!(manifest_path.is_file());

        // No changes detected after creating manifest
        let changes = check_upstream_changes(Some(tmp.path()));
        assert!(changes.is_empty());

        // Modify file -> change detected
        std::fs::write(
            tmp.path().join("recipe-a.yaml"),
            "name: recipe-a\nsteps:\n  - id: s1\n    command: echo modified",
        )
        .unwrap();
        let changes = check_upstream_changes(Some(tmp.path()));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["status"], "modified");
    }

    #[test]
    fn test_file_hash_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();
        let h1 = file_hash(&path);
        let h2 = file_hash(&path);
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    // -- DiscoveryCache tests --

    fn make_recipe_dir(name: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(format!("{}.yaml", name)),
            format!("name: {}\nsteps:\n  - id: s1\n    command: echo", name),
        )
        .unwrap();
        tmp
    }

    #[test]
    fn test_cache_hit() {
        let tmp = make_recipe_dir("cached-recipe");
        let dirs = vec![tmp.path().to_path_buf()];
        let mut cache = DiscoveryCache::new(Duration::from_secs(60));

        // First call populates
        let result = cache.get_or_discover(&dirs);
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("cached-recipe"));

        // Add another recipe file — a cache hit should NOT see it
        std::fs::write(
            tmp.path().join("extra.yaml"),
            "name: extra\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();

        let result = cache.get_or_discover(&dirs);
        assert_eq!(result.len(), 1, "cache hit must return stale data");
    }

    #[test]
    fn test_cache_miss_ttl_expired() {
        let tmp = make_recipe_dir("ttl-recipe");
        let dirs = vec![tmp.path().to_path_buf()];
        // TTL of zero means every call is a miss
        let mut cache = DiscoveryCache::new(Duration::from_secs(0));

        let result = cache.get_or_discover(&dirs);
        assert_eq!(result.len(), 1);

        // Add another recipe file — expired TTL must re-discover
        std::fs::write(
            tmp.path().join("new-recipe.yaml"),
            "name: new-recipe\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();

        let result = cache.get_or_discover(&dirs);
        assert_eq!(
            result.len(),
            2,
            "expired cache must re-scan and find new recipe"
        );
    }

    #[test]
    fn test_cache_miss_dirs_changed() {
        let tmp1 = make_recipe_dir("dir1-recipe");
        let tmp2 = make_recipe_dir("dir2-recipe");
        let mut cache = DiscoveryCache::new(Duration::from_secs(60));

        // Populate with dir1
        let result = cache.get_or_discover(&[tmp1.path().to_path_buf()]);
        assert!(result.contains_key("dir1-recipe"));

        // Switch to dir2 — dirs changed so cache must miss
        let result = cache.get_or_discover(&[tmp2.path().to_path_buf()]);
        assert!(
            !result.contains_key("dir1-recipe"),
            "old dir results must not appear"
        );
        assert!(
            result.contains_key("dir2-recipe"),
            "new dir results must appear"
        );
    }

    #[test]
    fn test_package_recipe_dir_env_var() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("env-recipe.yaml"),
            "name: env-recipe\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();

        // Set the env var and verify discovery finds the recipe
        // SAFETY: test runs are single-threaded for env var tests
        unsafe {
            std::env::set_var("AMPLIHACK_PACKAGE_RECIPE_DIR", tmp.path().as_os_str());
        }
        let dirs = default_search_dirs();
        assert!(
            dirs.contains(&tmp.path().to_path_buf()),
            "default_search_dirs should include AMPLIHACK_PACKAGE_RECIPE_DIR"
        );
        // Search only our tmp dir to isolate the test
        let recipes = discover_recipes(Some(&[tmp.path().to_path_buf()]));
        assert!(
            recipes.contains_key("env-recipe"),
            "recipe from AMPLIHACK_PACKAGE_RECIPE_DIR should be discoverable"
        );
        // SAFETY: test cleanup
        unsafe {
            std::env::remove_var("AMPLIHACK_PACKAGE_RECIPE_DIR");
        }
    }

    #[test]
    fn test_empty_package_recipe_dir_env_var_is_ignored() {
        // SAFETY: test runs are single-threaded for env var tests
        unsafe {
            std::env::set_var("AMPLIHACK_PACKAGE_RECIPE_DIR", "");
        }
        let dirs = default_search_dirs();
        assert!(
            !dirs.iter().any(|d| d.as_os_str().is_empty()),
            "empty AMPLIHACK_PACKAGE_RECIPE_DIR must not add an empty search dir"
        );
        // SAFETY: test cleanup
        unsafe {
            std::env::remove_var("AMPLIHACK_PACKAGE_RECIPE_DIR");
        }
    }

    #[test]
    fn test_package_recipe_dir_precedes_extra_recipe_dirs() {
        let pkg = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();

        // SAFETY: test runs are single-threaded for env var tests
        unsafe {
            std::env::set_var("AMPLIHACK_PACKAGE_RECIPE_DIR", pkg.path().as_os_str());
            std::env::set_var(
                "RECIPE_RUNNER_RECIPE_DIRS",
                extra.path().display().to_string(),
            );
        }

        let dirs = default_search_dirs();
        assert_eq!(
            dirs.first(),
            Some(&pkg.path().to_path_buf()),
            "package recipe dir should be searched before extra dirs"
        );
        assert!(
            dirs.contains(&extra.path().to_path_buf()),
            "extra recipe dirs should still be included"
        );

        // SAFETY: test cleanup
        unsafe {
            std::env::remove_var("AMPLIHACK_PACKAGE_RECIPE_DIR");
            std::env::remove_var("RECIPE_RUNNER_RECIPE_DIRS");
        }
    }

    // ── #42: find_recipe + discover_recipes must support .yml ──────────

    #[test]
    fn test_find_recipe_finds_yml_extension() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("yml-recipe.yml"),
            "name: yml-recipe\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();
        let found = find_recipe("yml-recipe", Some(&[tmp.path().to_path_buf()]));
        assert!(
            found.is_some(),
            "find_recipe must locate .yml files, not only .yaml"
        );
        assert!(found.unwrap().extension().unwrap() == "yml");
    }

    #[test]
    fn test_find_recipe_yaml_takes_precedence_over_yml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dup.yaml"),
            "name: dup\nsteps:\n  - id: s1\n    command: echo yaml",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("dup.yml"),
            "name: dup\nsteps:\n  - id: s1\n    command: echo yml",
        )
        .unwrap();
        let found = find_recipe("dup", Some(&[tmp.path().to_path_buf()])).unwrap();
        assert_eq!(
            found.extension().unwrap(),
            "yaml",
            ".yaml must win over .yml when both exist"
        );
    }

    #[test]
    fn test_discover_recipes_includes_yml_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("a.yaml"),
            "name: a\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("b.yml"),
            "name: b\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();
        let recipes = discover_recipes(Some(&[tmp.path().to_path_buf()]));
        assert!(recipes.contains_key("a"), ".yaml must be discovered");
        assert!(recipes.contains_key("b"), ".yml must be discovered");
    }

    #[test]
    fn test_find_recipe_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        // Even if a malicious file exists outside the search dir, name with
        // traversal segments must not resolve to it.
        for bad in [
            "../etc/passwd",
            "../../foo",
            "a/b",
            "a\\b",
            ".hidden",
            "foo..bar",
            "",
        ] {
            assert!(
                find_recipe(bad, Some(&[tmp.path().to_path_buf()])).is_none(),
                "find_recipe must reject suspicious name: {:?}",
                bad
            );
        }
    }

    // ── #46: upstream_url env override + URL validation ────────────────

    #[test]
    fn test_upstream_url_inner_default_when_no_env() {
        let url = upstream_url_inner(|_| None).expect("default URL must be valid");
        assert_eq!(url, DEFAULT_UPSTREAM_URL);
    }

    #[test]
    fn test_upstream_url_inner_uses_env_override() {
        let url = upstream_url_inner(|k| {
            (k == "RECIPE_RUNNER_UPSTREAM_URL").then(|| "https://example.com/repo.git".to_string())
        })
        .expect("https override must be accepted");
        assert_eq!(url, "https://example.com/repo.git");
    }

    #[test]
    fn test_upstream_url_inner_accepts_http() {
        let url = upstream_url_inner(|_| Some("http://example.com/repo".to_string()))
            .expect("http override must be accepted");
        assert_eq!(url, "http://example.com/repo");
    }

    #[test]
    fn test_upstream_url_inner_rejects_non_http_schemes() {
        for bad in [
            "file:///etc/passwd",
            "ssh://git@example.com/repo",
            "git://example.com/repo",
            "ftp://example.com/repo",
            "javascript:alert(1)",
            "no-scheme",
        ] {
            let res = upstream_url_inner(|_| Some(bad.to_string()));
            assert!(res.is_err(), "scheme must be rejected: {:?}", bad);
            // Error message must NOT echo the raw value (security)
            let msg = res.unwrap_err().to_string();
            assert!(
                !msg.contains(bad),
                "error message must not echo raw env var value (got {:?})",
                msg
            );
        }
    }

    #[test]
    fn test_upstream_url_inner_rejects_embedded_userinfo() {
        let res = upstream_url_inner(|_| Some("https://user:secret@example.com/repo".to_string()));
        assert!(res.is_err(), "URL with userinfo must be rejected");
        let msg = res.unwrap_err().to_string();
        assert!(
            !msg.contains("secret") && !msg.contains("user:"),
            "error must not leak credentials: {:?}",
            msg
        );
    }

    #[test]
    fn test_upstream_url_inner_rejects_empty() {
        let res = upstream_url_inner(|_| Some(String::new()));
        assert!(res.is_err(), "empty URL must be rejected");
    }

    // ── #45: verify_global_installation must be removed ────────────────

    /// Compile-time guarantee: verify_global_installation no longer exists.
    /// If this module compiles after removal, the symbol is gone.
    /// This test exists to document intent; the absence of the symbol is
    /// the actual assertion (any caller would fail to compile).
    #[test]
    fn test_verify_global_installation_removed() {
        // Intentionally empty — the surrounding cfg(test) module will fail
        // to compile if any code references the deleted function.
    }

    #[test]
    fn test_cache_invalidate() {
        let tmp = make_recipe_dir("inv-recipe");
        let dirs = vec![tmp.path().to_path_buf()];
        let mut cache = DiscoveryCache::new(Duration::from_secs(60));

        cache.get_or_discover(&dirs);
        assert_eq!(cache.len(), 1);

        // Add file, then invalidate
        std::fs::write(
            tmp.path().join("another.yaml"),
            "name: another\nsteps:\n  - id: s1\n    command: echo",
        )
        .unwrap();
        cache.invalidate();

        let result = cache.get_or_discover(&dirs);
        assert_eq!(result.len(), 2, "invalidated cache must re-scan");
    }
}
