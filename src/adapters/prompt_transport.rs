//! Attempt-owned input transport. Payloads deliberately have no Debug representation.
use anyhow::Context;
use std::{fs::OpenOptions, io::Write, path::Path};

/// Prepared before command construction and kept until child retirement.
pub(super) struct PromptTransport {
    pub(super) stdin: String,
    directory: Option<tempfile::TempDir>,
    pointer: Option<String>,
}

impl PromptTransport {
    pub(super) fn prepare(cli: &str, task: &str, system: &str) -> anyhow::Result<Self> {
        let envelope = || format!("# System instructions\n{system}\n\n# Task instructions\n{task}");
        if cli == "codex" {
            return Ok(Self {
                stdin: envelope(),
                directory: None,
                pointer: None,
            });
        }
        let directory = tempfile::Builder::new()
            .prefix("amplihack-agent-prompt-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(
                std::path::absolute(std::env::temp_dir())
                    .context("Failed to resolve prompt directory")?,
            )
            .context("Failed to create private prompt directory")?;
        let mut transport = Self {
            stdin: if Self::claude(cli) {
                task.to_owned()
            } else {
                String::new()
            },
            directory: Some(directory),
            pointer: None,
        };
        let preparation = (|| {
            let path = transport.file().context("Missing prompt file path")?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .context("Failed to create private prompt file")?;
            let content = if Self::claude(cli) {
                system.to_owned()
            } else {
                envelope()
            };
            file.write_all(content.as_bytes())
                .context("Failed to write private prompt file")?;
            file.flush()
                .context("Failed to flush private prompt file")?;
            drop(file);
            if !Self::claude(cli) {
                transport.pointer = Some(format!(
                    "Read the complete system and task instructions in {} and follow them fully.",
                    path.display()
                ));
            }
            Ok(())
        })();
        if let Err(error) = preparation {
            return super::codex_exec::combine_result(Err(error), transport.cleanup());
        }
        Ok(transport)
    }

    fn claude(cli: &str) -> bool {
        matches!(cli, "claude" | "launch" | "RustyClawd")
    }

    pub(super) fn directory(&self) -> Option<&Path> {
        self.directory.as_ref().map(tempfile::TempDir::path)
    }

    fn file(&self) -> Option<std::path::PathBuf> {
        self.directory()
            .map(|directory| directory.join("instructions.md"))
    }

    /// Only generated paths and fixed pointer wording may reach argv.
    pub(super) fn configure(&self, command: &mut std::process::Command) {
        if let Some(pointer) = &self.pointer {
            command
                .arg("-p")
                .arg(pointer)
                .arg("--add-dir")
                .arg(self.directory().expect("pointer owns directory"));
        } else if let Some(file) = self.file() {
            command.arg("--system-prompt-file").arg(file).arg("-p");
        }
    }

    /// Explicit removal reports failures; retained ownership supplies a Drop fallback.
    pub(super) fn cleanup(&mut self) -> anyhow::Result<()> {
        if let Some(directory) = &self.directory {
            std::fs::remove_dir_all(directory.path())
                .context("Failed to clean private prompt resources")?;
            self.directory.take();
        }
        Ok(())
    }
}

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

impl Drop for PromptTransport {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            log::error!("{error:#}");
        }
    }
}
