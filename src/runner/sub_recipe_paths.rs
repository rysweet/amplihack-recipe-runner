//! Anchored search-path resolution for sub-recipes.
//!
//! When a top-level recipe references a sub-recipe by name, the runner needs
//! to find that sub-recipe in directories that are *anchored* to the location
//! the user invoked the recipe from — not just the global default search
//! directories. This module provides:
//!
//! * [`validate_sub_recipe_name`] — security primitive that rejects names
//!   containing path separators, parent-dir markers, or absolute prefixes.
//! * [`is_within_any`] — canonicalized containment check that prevents
//!   symlink-based escapes from a candidate file.
//! * [`walk_up_to_git`] — bounded ascent (10 ancestors) from a starting
//!   directory toward a `.git` marker, returning intermediate ancestors.
//! * [`anchored_search_dirs`] — composes the ordered list of dirs to search:
//!   recipe-local → working_dir/amplifier-bundle/recipes → walk-up
//!   ancestors/amplifier-bundle/recipes.
//!
//! These helpers are intentionally pure (no I/O beyond filesystem stat) so
//! they can be unit-tested in isolation.

use std::path::{Path, PathBuf};

/// Maximum number of ancestor directories inspected when walking up looking
/// for a `.git` marker. Bounded to defend against pathological symlink loops
/// and runaway traversal on systems with deeply nested mounts.
pub(crate) const WALK_UP_MAX_ANCESTORS: usize = 10;

/// Reject sub-recipe names that could escape the search directory.
///
/// Acceptance: `^[A-Za-z0-9_-]+$`. This is intentionally stricter than
/// [`crate::discovery::find_recipe`]'s `is_safe_recipe_name` because callers
/// at the runner layer are programmatic (recipe authors writing
/// `recipe: "<name>"` step fields) and have no legitimate reason to use
/// dotted names, mixed extensions, or anything else exotic.
///
/// Returns `Ok(())` if the name is acceptable, or `Err(reason)` otherwise.
/// The error string is suitable for inclusion in a Zero-BS diagnostic.
pub(crate) fn validate_sub_recipe_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("sub-recipe name is empty");
    }
    if name.starts_with('.') {
        return Err("sub-recipe name must not start with '.'");
    }
    if name.contains("..") {
        return Err("sub-recipe name must not contain '..'");
    }
    for c in name.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '_' || c == '-';
        if !ok {
            return Err(
                "sub-recipe name must match [A-Za-z0-9_-]+ (no path separators, NUL bytes, or whitespace)",
            );
        }
    }
    Ok(())
}

/// Return true if `candidate` resolves (after canonicalization) to a path
/// inside any of `roots`. Used to guarantee that a resolved sub-recipe file
/// physically lives within one of the anchored search directories — even if
/// a symlink in that directory points elsewhere.
///
/// If either side fails to canonicalize (e.g., the file does not exist),
/// returns `false` — the caller should treat that as "not found within any
/// permitted root" and continue looking.
pub(crate) fn is_within_any(candidate: &Path, roots: &[PathBuf]) -> bool {
    let canonical_candidate = match candidate.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    for root in roots {
        if let Ok(canonical_root) = root.canonicalize()
            && canonical_candidate.starts_with(&canonical_root)
        {
            return true;
        }
    }
    false
}

/// Walk up from `start` collecting ancestor directories, stopping when a
/// `.git` marker is encountered (the walk includes that ancestor) or when
/// [`WALK_UP_MAX_ANCESTORS`] is reached.
///
/// The walk is bounded to defend against pathological symlink loops and
/// avoids ascending into the filesystem root indefinitely.
///
/// Returns ancestors in order from `start` outward. `start` itself is the
/// first element if it is a directory.
pub(crate) fn walk_up_to_git(start: &Path) -> Vec<PathBuf> {
    let mut acc = Vec::new();
    let mut current = match start.canonicalize() {
        Ok(p) => p,
        Err(_) => start.to_path_buf(),
    };

    for _ in 0..WALK_UP_MAX_ANCESTORS {
        acc.push(current.clone());
        if current.join(".git").exists() {
            return acc;
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }
    acc
}

/// Compose the ordered list of anchored sub-recipe search directories.
///
/// Order:
///   1. `recipe_origin_dir` — directory containing the top-level recipe file
///      that was loaded at runner entry. Sub-recipes co-located with their
///      parent recipe are the most natural reference.
///   2. `working_dir/amplifier-bundle/recipes` — project-relative recipes
///      anchored at the user-supplied `-C` working directory (NOT the
///      runner subprocess's actual cwd, which can drift).
///   3. For each ancestor of `working_dir` discovered by [`walk_up_to_git`]:
///      `<ancestor>/amplifier-bundle/recipes`. This catches the case where
///      the user invokes the runner from a subdirectory of a repo that has
///      `amplifier-bundle/recipes` at its root.
///
/// Duplicates are de-duplicated while preserving first-seen order.
/// Non-directory entries are filtered out.
pub(crate) fn anchored_search_dirs(
    recipe_origin_dir: Option<&Path>,
    working_dir: &Path,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let push_unique = |dirs: &mut Vec<PathBuf>, p: PathBuf| {
        if p.is_dir() && !dirs.iter().any(|existing| existing == &p) {
            dirs.push(p);
        }
    };

    if let Some(origin) = recipe_origin_dir {
        push_unique(&mut dirs, origin.to_path_buf());
    }

    // The runner's working_dir itself, so that a bare `<name>.yaml` next
    // to the working dir is discoverable (preserves a niche fallback from
    // the pre-#480 implementation while keeping it inside the
    // canonicalized-containment perimeter).
    push_unique(&mut dirs, working_dir.to_path_buf());

    let working_bundle = working_dir.join("amplifier-bundle").join("recipes");
    push_unique(&mut dirs, working_bundle);

    for ancestor in walk_up_to_git(working_dir) {
        let candidate = ancestor.join("amplifier-bundle").join("recipes");
        push_unique(&mut dirs, candidate);
    }

    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_normal_names() {
        for name in ["smart-classify-route", "workflow_prep", "abc123", "x"] {
            assert!(
                validate_sub_recipe_name(name).is_ok(),
                "must accept: {name}"
            );
        }
    }

    #[test]
    fn validate_rejects_path_traversal() {
        for bad in [
            "",
            ".hidden",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            "a..b",
            "a b",
        ] {
            assert!(
                validate_sub_recipe_name(bad).is_err(),
                "must reject: {bad:?}"
            );
        }
    }

    #[test]
    fn validate_rejects_nul_byte() {
        assert!(validate_sub_recipe_name("a\0b").is_err());
    }

    #[test]
    fn anchored_dirs_includes_recipe_origin_first() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();

        let dirs = anchored_search_dirs(Some(&origin), &work);
        assert_eq!(dirs.first().unwrap(), &origin);
    }

    #[test]
    fn anchored_dirs_includes_working_dir_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().to_path_buf();
        let bundle = work.join("amplifier-bundle").join("recipes");
        std::fs::create_dir_all(&bundle).unwrap();

        let dirs = anchored_search_dirs(None, &work);
        assert!(dirs.contains(&bundle), "got: {dirs:?}");
    }

    #[test]
    fn anchored_dirs_walks_up_to_repo_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path();
        std::fs::create_dir_all(repo_root.join(".git")).unwrap();
        let bundle = repo_root.join("amplifier-bundle").join("recipes");
        std::fs::create_dir_all(&bundle).unwrap();
        let nested = repo_root.join("crates").join("subproject");
        std::fs::create_dir_all(&nested).unwrap();

        let dirs = anchored_search_dirs(None, &nested);
        let canonical_bundle = bundle.canonicalize().unwrap();
        assert!(
            dirs.iter()
                .any(|d| d.canonicalize().ok() == Some(canonical_bundle.clone())),
            "expected walk-up to find repo-root bundle; got: {dirs:?}"
        );
    }

    #[test]
    fn anchored_dirs_dedupes() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().to_path_buf();
        let bundle = work.join("amplifier-bundle").join("recipes");
        std::fs::create_dir_all(&bundle).unwrap();

        // origin == working_bundle would otherwise produce a duplicate
        let dirs = anchored_search_dirs(Some(&bundle), &work);
        let count = dirs.iter().filter(|d| *d == &bundle).count();
        assert_eq!(count, 1, "must dedupe; got: {dirs:?}");
    }

    #[test]
    fn walk_up_bounded_at_max_ancestors() {
        // Build a deeply nested structure with no .git marker and verify
        // we stop after WALK_UP_MAX_ANCESTORS ascents.
        let tmp = tempfile::tempdir().unwrap();
        let mut path = tmp.path().to_path_buf();
        for i in 0..(WALK_UP_MAX_ANCESTORS + 5) {
            path = path.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&path).unwrap();
        let walk = walk_up_to_git(&path);
        assert!(
            walk.len() <= WALK_UP_MAX_ANCESTORS,
            "walk_up_to_git must be bounded; got {} ancestors",
            walk.len()
        );
    }

    #[test]
    fn walk_up_stops_at_git_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        let walk = walk_up_to_git(&nested);
        let canonical_repo = repo.canonicalize().unwrap();
        let last = walk.last().unwrap().canonicalize().unwrap();
        assert_eq!(last, canonical_repo, "must stop at .git marker");
    }

    #[test]
    fn is_within_any_accepts_real_containment() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let file = root.join("foo.yaml");
        std::fs::write(&file, "x").unwrap();
        assert!(is_within_any(&file, &[root]));
    }

    #[test]
    fn is_within_any_rejects_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_within_any(
            &tmp.path().join("does-not-exist.yaml"),
            &[tmp.path().to_path_buf()]
        ));
    }

    #[test]
    #[cfg(unix)]
    fn is_within_any_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("permitted");
        std::fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.yaml");
        std::fs::write(&secret, "x").unwrap();

        // Symlink inside `root` that points outside.
        let link = root.join("escape.yaml");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        assert!(
            !is_within_any(&link, &[root]),
            "containment check must canonicalize and reject symlink escapes"
        );
    }
}
