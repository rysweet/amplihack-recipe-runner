use recipe_runner_rs::discovery::cached_discover_recipes;
use std::path::PathBuf;

#[test]
fn cached_discover_returns_consistent_results() {
    let dirs = vec![PathBuf::from("recipes"), PathBuf::from("examples")];
    let first = cached_discover_recipes(&dirs);
    let second = cached_discover_recipes(&dirs);
    // Cache should return identical results
    assert_eq!(first.len(), second.len());
    for (key, info) in &first {
        let other = second.get(key).expect("key missing from cached result");
        assert_eq!(info.name, other.name);
        assert_eq!(info.path, other.path);
    }
}

#[test]
fn cached_discover_empty_dirs() {
    let dirs: Vec<PathBuf> = vec![PathBuf::from("/nonexistent_dir_abc123")];
    let result = cached_discover_recipes(&dirs);
    assert!(result.is_empty());
}
