use recipe_runner_rs::discovery::{cached_discover_recipes, verify_global_installation};
use std::path::PathBuf;

#[test]
fn verify_global_returns_expected_fields() {
    let result = verify_global_installation();
    assert!(result.is_object());
    let obj = result.as_object().unwrap();
    assert!(obj.contains_key("global_dirs_exist"));
    assert!(obj.contains_key("global_recipe_count"));
    assert!(obj.contains_key("has_global_recipes"));
    assert!(obj.contains_key("global_paths_checked"));

    // global_dirs_exist and global_recipe_count must be arrays
    assert!(obj["global_dirs_exist"].is_array());
    assert!(obj["global_recipe_count"].is_array());
    assert!(obj["has_global_recipes"].is_boolean());
    assert!(obj["global_paths_checked"].is_array());
}

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
