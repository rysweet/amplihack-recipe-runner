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

fn default_search_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    vec![
        home.join(".amplihack").join(".claude").join("recipes"),
        PathBuf::from("amplifier-bundle").join("recipes"),
        PathBuf::from("src")
            .join("amplihack")
            .join("amplifier-bundle")
            .join("recipes"),
        PathBuf::from(".claude").join("recipes"),
    ]
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

/// Find all recipe YAML files in the search directories.
///
/// When the same recipe name appears in multiple directories, the last one wins.
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

        let mut entries: Vec<PathBuf> = std::fs::read_dir(search_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "yaml"))
            .collect();
        entries.sort();

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
    pub cache: HashMap<String, RecipeInfo>,
    pub last_updated: Instant,
    pub ttl: Duration,
    pub search_dirs: Vec<PathBuf>,
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
}

/// Thread-safe convenience wrapper around [`DiscoveryCache`].
///
/// Uses a module-level `Mutex<DiscoveryCache>` so callers don't need to manage
/// their own cache instance.  The default TTL is 30 seconds.
pub fn cached_discover_recipes(dirs: &[PathBuf]) -> HashMap<String, RecipeInfo> {
    static CACHE: std::sync::LazyLock<Mutex<DiscoveryCache>> =
        std::sync::LazyLock::new(|| Mutex::new(DiscoveryCache::new(Duration::from_secs(30))));

    let mut cache = CACHE.lock().expect("DiscoveryCache mutex poisoned");
    cache.get_or_discover(dirs).clone()
}

/// Find a recipe by name and return its file path.
pub fn find_recipe(name: &str, search_dirs: Option<&[PathBuf]>) -> Option<PathBuf> {
    let dirs = search_dirs
        .map(|d| d.to_vec())
        .unwrap_or_else(default_search_dirs);
    let filename = format!("{}.yaml", name);
    for search_dir in &dirs {
        let candidate = search_dir.join(&filename);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Verify that global recipe directories exist and contain recipes.
pub fn verify_global_installation() -> serde_json::Value {
    let global_dirs = vec![
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".amplihack")
            .join(".claude")
            .join("recipes"),
        PathBuf::from("amplifier-bundle").join("recipes"),
    ];

    let mut dirs_exist = Vec::new();
    let mut recipe_counts = Vec::new();
    let mut has_global = false;

    for dir in &global_dirs {
        let exists = dir.is_dir();
        dirs_exist.push(exists);
        if exists {
            let count = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "yaml"))
                .count();
            recipe_counts.push(count);
            if count > 0 {
                has_global = true;
            }
        } else {
            recipe_counts.push(0);
        }
    }

    serde_json::json!({
        "global_dirs_exist": dirs_exist,
        "global_recipe_count": recipe_counts,
        "has_global_recipes": has_global,
        "global_paths_checked": global_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    })
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
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&recipe_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
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

/// Write a manifest file recording the current hash of each recipe.
pub fn update_manifest(local_dir: Option<&Path>) -> Result<PathBuf, std::io::Error> {
    let recipe_dir = local_dir
        .map(|p| p.to_path_buf())
        .or_else(find_first_recipe_dir)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "No recipe directory found")
        })?;

    let mut manifest = HashMap::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&recipe_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    entries.sort();

    for yaml_path in &entries {
        if let Some(stem) = yaml_path.file_stem().and_then(|s| s.to_str()) {
            manifest.insert(stem.to_string(), file_hash(yaml_path));
        }
    }

    let manifest_path = recipe_dir.join("_recipe_manifest.json");
    let json = serde_json::to_string_pretty(&manifest).unwrap_or_default();
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
    let repo = repo_url.unwrap_or("https://github.com/microsoft/amplifier-bundle-recipes");
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

    // Fetch
    let fetch_output = Command::new("git").args(["fetch", &remote, br]).output()?;
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

    Ok(serde_json::json!({
        "has_changes": has_changes,
        "files_changed": files_changed,
        "diff_summary": if has_changes { crate::safe_truncate(&diff_stdout, 500) } else { "No changes" },
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
            hex::encode(&result[..8]) // First 16 hex chars = 8 bytes
        }
        Err(_) => String::new(),
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
    fn test_cache_invalidate() {
        let tmp = make_recipe_dir("inv-recipe");
        let dirs = vec![tmp.path().to_path_buf()];
        let mut cache = DiscoveryCache::new(Duration::from_secs(60));

        cache.get_or_discover(&dirs);
        assert_eq!(cache.cache.len(), 1);

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
