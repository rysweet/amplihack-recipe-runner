use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tar::Archive;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_REPO: &str = "rysweet/amplihack-recipe-runner";
const NO_UPDATE_CHECK_ENV: &str = "RECIPE_RUNNER_NO_UPDATE_CHECK";
const UPDATE_CACHE_RELATIVE_PATH: &str = ".config/recipe-runner-rs/last_update_check";
const UPDATE_CHECK_COOLDOWN_SECS: u64 = 24 * 60 * 60;
const NETWORK_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpdateRelease {
    version: String,
    asset_url: String,
}

/// Check for updates on startup and print a notice if one is available.
/// Respects 24-hour cooldown and RECIPE_RUNNER_NO_UPDATE_CHECK=1.
/// Skips check for help/update/version subcommands.
pub fn maybe_print_update_notice_from_args(args: &[OsString]) {
    if should_skip_update_check(args) || supported_release_target().is_none() {
        return;
    }

    if let Err(error) = maybe_print_update_notice() {
        log::debug!("startup update check skipped: {error}");
    }
}

/// Run the self-update: check for a new version, download, and replace the binary.
pub fn run_update() -> Result<()> {
    println!("recipe-runner-rs update (current: v{CURRENT_VERSION})");

    let release = fetch_latest_release()?;
    if !is_newer(CURRENT_VERSION, &release.version)? {
        println!("Already at the latest version (v{CURRENT_VERSION}).");
        return Ok(());
    }

    println!(
        "New version available: v{} -> v{}",
        CURRENT_VERSION, release.version
    );
    download_and_replace(&release)?;
    write_cache(&cache_path()?, &release.version)?;
    Ok(())
}

fn maybe_print_update_notice() -> Result<()> {
    if std::env::var(NO_UPDATE_CHECK_ENV).unwrap_or_default() == "1" {
        return Ok(());
    }

    let cache_path = cache_path()?;
    let now = now_secs();

    if let Some((cached_version, timestamp)) = read_cache(&cache_path)
        && now.saturating_sub(timestamp) < UPDATE_CHECK_COOLDOWN_SECS
    {
        if is_newer(CURRENT_VERSION, &cached_version)? {
            print_update_notice(&cached_version);
        }
        return Ok(());
    }

    let release = fetch_latest_release()?;
    write_cache(&cache_path, &release.version)?;
    if is_newer(CURRENT_VERSION, &release.version)? {
        print_update_notice(&release.version);
    }
    Ok(())
}

fn fetch_latest_release() -> Result<UpdateRelease> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let asset_name = expected_archive_name()?;
    let response = http_get(&url)
        .with_context(|| format!("failed to query latest stable release from {GITHUB_REPO}"))?;
    parse_latest_release(response, &asset_name)
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    let timeout = Duration::from_secs(NETWORK_TIMEOUT_SECS);
    let response = match ureq::AgentBuilder::new()
        .timeout_connect(timeout)
        .timeout_read(timeout)
        .timeout_write(timeout)
        .build()
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .set(
            "User-Agent",
            &format!("recipe-runner-rs/{CURRENT_VERSION}"),
        )
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(404, _)) if url.ends_with("/releases/latest") => {
            bail!("no stable v* release has been published for {GITHUB_REPO} yet")
        }
        Err(error) => return Err(anyhow!("HTTP request failed for {url}: {error}")),
    };

    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .with_context(|| format!("failed to read HTTP response from {url}"))?;
    Ok(body)
}

fn parse_latest_release(body: Vec<u8>, asset_name: &str) -> Result<UpdateRelease> {
    let release: GithubRelease =
        serde_json::from_slice(&body).context("failed to parse GitHub release JSON")?;

    if release.draft {
        bail!("latest release is unexpectedly marked as draft");
    }
    if release.prerelease {
        bail!("latest stable release endpoint returned a prerelease");
    }

    let version = normalize_tag(&release.tag_name)?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| {
            anyhow!(
                "release {} does not contain asset {}",
                release.tag_name,
                asset_name
            )
        })?;

    Ok(UpdateRelease {
        version,
        asset_url: asset.browser_download_url.clone(),
    })
}

fn normalize_tag(tag: &str) -> Result<String> {
    let trimmed = tag.trim().trim_start_matches('v');
    Version::parse(trimmed).with_context(|| format!("release tag is not valid semver: {tag}"))?;
    Ok(trimmed.to_string())
}

fn supported_release_target() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else {
        None
    }
}

fn required_release_target() -> Result<&'static str> {
    supported_release_target().ok_or_else(|| {
        anyhow!(
            "self-update is only supported on linux/macos (x86_64 and aarch64)"
        )
    })
}

fn expected_archive_name() -> Result<String> {
    Ok(format!(
        "recipe-runner-rs-{}.tar.gz",
        required_release_target()?
    ))
}

fn is_newer(current: &str, latest: &str) -> Result<bool> {
    let current = Version::parse(current)
        .with_context(|| format!("current version is not valid semver: {current}"))?;
    let latest = Version::parse(latest)
        .with_context(|| format!("latest version is not valid semver: {latest}"))?;
    Ok(latest > current)
}

fn cache_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(cache_path_from_home(Path::new(&home)))
}

fn cache_path_from_home(home: &Path) -> PathBuf {
    home.join(UPDATE_CACHE_RELATIVE_PATH)
}

fn read_cache(path: &Path) -> Option<(String, u64)> {
    let content = fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let version = lines.next()?.to_string();
    let timestamp = lines.next()?.parse().ok()?;
    Some((version, timestamp))
}

fn write_cache(path: &Path, version: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, format!("{}\n{}", version, now_secs()))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn should_skip_update_check(args: &[OsString]) -> bool {
    if std::env::var(NO_UPDATE_CHECK_ENV).unwrap_or_default() == "1" {
        return true;
    }

    let first_arg = args.get(1).and_then(|arg| arg.to_str());
    matches!(
        first_arg,
        None | Some("help")
            | Some("update")
            | Some("version")
            | Some("-h")
            | Some("--help")
            | Some("-V")
            | Some("--version")
    )
}

fn print_update_notice(latest: &str) {
    eprintln!(
        "\x1b[33mA newer version of recipe-runner-rs is available (v{latest}). Run 'recipe-runner-rs update' to upgrade.\x1b[0m"
    );
}

fn download_and_replace(release: &UpdateRelease) -> Result<()> {
    let archive_bytes = http_get(&release.asset_url)?;

    // Try SHA256 verification if checksum file is available
    let sha_url = format!("{}.sha256", release.asset_url);
    if let Ok(sha_bytes) = http_get(&sha_url) {
        let expected_hex = String::from_utf8_lossy(&sha_bytes);
        let expected_hex = expected_hex.split_whitespace().next().unwrap_or("");
        if !expected_hex.is_empty() {
            let actual_hex = hex::encode(Sha256::digest(&archive_bytes));
            if actual_hex != expected_hex {
                bail!(
                    "SHA256 mismatch: expected {expected_hex}, got {actual_hex}"
                );
            }
            println!("SHA256 verified.");
        }
    }

    let temp_dir = tempfile::tempdir().context("failed to create update temp directory")?;
    extract_archive(&archive_bytes, temp_dir.path())?;

    let binary_name = if cfg!(windows) {
        "recipe-runner-rs.exe"
    } else {
        "recipe-runner-rs"
    };
    let new_binary = find_binary(temp_dir.path(), binary_name)?;
    let current_exe =
        std::env::current_exe().context("cannot determine current executable path")?;

    install_binary_atomic(&new_binary, &current_exe)?;

    println!(
        "Updated recipe-runner-rs: v{} -> v{}",
        CURRENT_VERSION, release.version
    );
    println!("Restart recipe-runner-rs to use the new version.");
    Ok(())
}

fn extract_archive(archive_bytes: &[u8], destination: &Path) -> Result<()> {
    let decoder = GzDecoder::new(std::io::Cursor::new(archive_bytes));
    let mut archive = Archive::new(decoder);
    archive
        .unpack(destination)
        .with_context(|| format!("failed to unpack archive into {}", destination.display()))?;
    Ok(())
}

fn find_binary(root: &Path, binary_name: &str) -> Result<PathBuf> {
    fn search(root: &Path, binary_name: &str, depth: usize) -> Option<PathBuf> {
        if depth > 3 {
            return None;
        }
        let entries = fs::read_dir(root).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path.file_name() == Some(std::ffi::OsStr::new(binary_name))
            {
                return Some(path);
            }
            if path.is_dir()
                && let Some(found) = search(&path, binary_name, depth + 1)
            {
                return Some(found);
            }
        }
        None
    }

    search(root, binary_name, 0)
        .ok_or_else(|| anyhow!("binary '{binary_name}' not found in downloaded archive"))
}

fn install_binary_atomic(source: &Path, destination: &Path) -> Result<()> {
    let temp_destination = destination.with_extension("tmp");
    fs::copy(source, &temp_destination).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            temp_destination.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp_destination, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to chmod {}", temp_destination.display()))?;
    }

    fs::rename(&temp_destination, destination).with_context(|| {
        format!(
            "failed to replace {} with {}",
            destination.display(),
            temp_destination.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tag_strips_v_prefix() {
        assert_eq!(normalize_tag("v1.2.3").unwrap(), "1.2.3");
    }

    #[test]
    fn normalize_tag_rejects_non_semver() {
        assert!(normalize_tag("snapshot-abcdef").is_err());
    }

    #[test]
    fn is_newer_detects_version_bumps() {
        assert!(is_newer("0.1.0", "0.2.0").unwrap());
        assert!(!is_newer("0.2.0", "0.2.0").unwrap());
        assert!(!is_newer("0.2.1", "0.2.0").unwrap());
    }

    #[test]
    fn should_skip_for_update_related_args() {
        assert!(should_skip_update_check(&[
            OsString::from("recipe-runner-rs"),
            OsString::from("update")
        ]));
        assert!(should_skip_update_check(&[
            OsString::from("recipe-runner-rs"),
            OsString::from("--version")
        ]));
        assert!(should_skip_update_check(&[
            OsString::from("recipe-runner-rs"),
            OsString::from("help")
        ]));
        assert!(!should_skip_update_check(&[
            OsString::from("recipe-runner-rs"),
            OsString::from("list")
        ]));
    }

    #[test]
    fn cache_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache");
        write_cache(&path, "1.2.3").unwrap();
        let (version, timestamp) = read_cache(&path).unwrap();
        assert_eq!(version, "1.2.3");
        assert!(timestamp > 0);
    }

    #[test]
    fn cache_path_uses_home() {
        let temp = tempfile::tempdir().unwrap();
        let path = cache_path_from_home(temp.path());
        assert_eq!(
            path,
            temp.path().join(".config/recipe-runner-rs/last_update_check")
        );
    }

    #[test]
    fn parse_latest_release_selects_matching_asset() {
        let asset_name = expected_archive_name().unwrap();
        let json = format!(
            r#"{{
                "tag_name": "v0.3.0",
                "draft": false,
                "prerelease": false,
                "assets": [
                    {{"name": "wrong.tar.gz", "browser_download_url": "https://example.invalid/wrong"}},
                    {{"name": "{asset_name}", "browser_download_url": "https://example.invalid/right"}}
                ]
            }}"#
        );
        let release = parse_latest_release(json.into_bytes(), &asset_name).unwrap();
        assert_eq!(
            release,
            UpdateRelease {
                version: "0.3.0".to_string(),
                asset_url: "https://example.invalid/right".to_string(),
            }
        );
    }

    #[test]
    fn current_platform_has_release_target() {
        assert!(supported_release_target().is_some());
    }

    #[test]
    fn extract_archive_finds_binary() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("release.tar.gz");
        create_test_archive(&archive_path).unwrap();
        let bytes = fs::read(&archive_path).unwrap();

        let extract_dir = temp.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        extract_archive(&bytes, &extract_dir).unwrap();

        let binary_name = if cfg!(windows) {
            "recipe-runner-rs.exe"
        } else {
            "recipe-runner-rs"
        };
        assert!(find_binary(&extract_dir, binary_name).is_ok());
    }

    fn create_test_archive(path: &Path) -> Result<()> {
        let tar_gz = fs::File::create(path)?;
        let encoder = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let binary_name = if cfg!(windows) {
            "recipe-runner-rs.exe"
        } else {
            "recipe-runner-rs"
        };
        let data = b"#!/bin/sh\nexit 0\n";
        let mut header = tar::Header::new_gnu();
        header.set_path(binary_name)?;
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, &data[..])?;

        let encoder = builder.into_inner()?;
        encoder.finish()?;
        Ok(())
    }
}
