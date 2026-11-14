//! Public CLI context contract: JSON types, precedence, failures, and legacy --set.
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn command(root: &Path) -> Command {
    let recipe = root.join("recipe.yaml");
    fs::write(
        &recipe,
        "name: context-contract\ncontext:\n  default: kept\n  overridden: recipe\nsteps:\n  - id: marker\n    command: touch executed\n    auto_stage: false\n",
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"));
    command
        .arg(recipe)
        .args([
            "--no-update-check",
            "--no-auto-stage",
            "--output-format",
            "json",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .current_dir(root);
    command
}

fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn context_file_preserves_json_types_and_set_overrides_file_and_defaults() {
    let root = tempfile::tempdir().unwrap();
    let values = json!({
        "text": " \t--private λ🦀\n\n ", "numeric_string": "42", "bool": true,
        "number": 42, "float": 1.25, "null": null,
        "array": ["a", 2, false], "object": {"nested": "value"}, "overridden": "file"
    });
    let path = root.path().join("context.json");
    private_file(&path, values.to_string().as_bytes());
    // --set precedence does not depend on its position relative to the file.
    for file_first in [true, false] {
        let mut command = command(root.path());
        if file_first {
            command.arg("--context-file").arg(&path);
        }
        command.args(["--set", "overridden=cli", "--set", "extra=99"]);
        if !file_first {
            command.arg("--context-file").arg(&path);
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        for (key, value) in values.as_object().unwrap() {
            if key != "overridden" {
                assert_eq!(&result["context"][key], value);
            }
        }
        assert_eq!(result["context"]["overridden"], "cli");
        assert_eq!(result["context"]["extra"], 99);
        assert_eq!(result["context"]["default"], "kept");
        assert_eq!(fs::read(&path).unwrap(), values.to_string().as_bytes());
    }
}

#[test]
fn legacy_set_without_a_file_retains_smart_parsing_and_malformed_pair_behavior() {
    let root = tempfile::tempdir().unwrap();
    let out = command(root.path())
        .args([
            "--set",
            "text=hello=world",
            "--set",
            "number=42",
            "--set",
            "bool=True",
            "--set",
            "object={\"a\":1}",
            "--set",
            "array=[1,2]",
            "--set",
            "empty=",
            "--set",
            "repeated=first",
            "--set",
            "repeated=last",
            "--set",
            "malformed",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        result["context"],
        json!({
            "default": "kept", "overridden": "recipe", "text": "hello=world", "number": 42,
            "bool": true, "object": {"a": 1}, "array": [1,2], "empty": "", "repeated": "last"
        })
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("ignoring malformed context override"));
}

#[test]
fn invalid_context_files_fail_before_execution_without_disclosing_contents() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("context.json");
    for bytes in [
        b"{\"PRIVATE_SENTINEL\": NaN}".as_slice(),
        b"[\"PRIVATE_SENTINEL\"]",
        b"\"PRIVATE_SENTINEL\"",
        b"null",
        b"42",
        b"",
        b"{} trailing",
    ] {
        private_file(&path, bytes);
        let out = command(root.path())
            .arg("--context-file")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4));
        assert!(out.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("Failed to read --context-file"));
        assert!(!stderr.contains("PRIVATE_SENTINEL"));
        assert!(!root.path().join("executed").exists());
    }
    for missing_or_directory in [root.path().join("missing"), root.path().to_owned()] {
        let out = command(root.path())
            .arg("--context-file")
            .arg(missing_or_directory)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4));
        assert!(out.stdout.is_empty());
        assert!(!root.path().join("executed").exists());
    }
}

#[cfg(unix)]
#[test]
fn context_file_rejects_public_permissions_symlinks_and_fifos() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("context.json");
    private_file(&path, b"{}");
    for mode in [0o644, 0o640, 0o606] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let out = command(root.path())
            .arg("--context-file")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(
        command(root.path())
            .arg("--context-file")
            .arg(&path)
            .output()
            .unwrap()
            .status
            .success()
    );
    let link = root.path().join("link");
    symlink(&path, &link).unwrap();
    let fifo = root.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: valid path, creating an owned fixture FIFO with private mode.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for rejected in [link, fifo] {
        let out = command(root.path())
            .arg("--context-file")
            .arg(rejected)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4));
        assert!(out.stdout.is_empty());
    }
}
