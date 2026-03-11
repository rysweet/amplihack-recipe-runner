/// Agent resolver for mapping agent references to system prompt content.
///
/// Resolves references like `amplihack:builder` or `amplihack:core:architect`
/// to the markdown content of the corresponding agent definition file.
///
use regex::Regex;
use std::path::PathBuf;
use std::sync::LazyLock;
use log::{debug, info, warn};

static SAFE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap());

#[derive(Debug, thiserror::Error)]
pub enum AgentResolveError {
    #[error("Agent '{agent_ref}' not found. Searched: {searched}")]
    NotFound { agent_ref: String, searched: String },

    #[error("Invalid agent reference: {0}")]
    InvalidReference(String),
}

fn default_search_paths() -> Vec<PathBuf> {
    debug!("default_search_paths: building default agent search paths");
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    debug!("default_search_paths: home={:?}", home);
    vec![
        home.join(".amplihack").join(".claude").join("agents"),
        PathBuf::from(".claude").join("agents"),
        PathBuf::from("amplifier-bundle").join("agents"),
        PathBuf::from("src")
            .join("amplihack")
            .join("amplifier-bundle")
            .join("agents"),
        PathBuf::from("src")
            .join("amplihack")
            .join(".claude")
            .join("agents"),
    ]
}

/// Resolves `namespace:name` agent references to their markdown content.
pub struct AgentResolver {
    search_paths: Vec<PathBuf>,
}

impl AgentResolver {
    pub fn new(search_paths: Option<Vec<PathBuf>>) -> Self {
        debug!("AgentResolver::new: initializing with {} search path(s)", search_paths.as_ref().map(|p| p.len()).unwrap_or(0));
        Self {
            search_paths: search_paths.unwrap_or_else(default_search_paths),
        }
    }

    /// Resolve an agent reference to its system prompt content.
    ///
    /// Accepts `namespace:name` or `namespace:category:name` format.
    pub fn resolve(&self, agent_ref: &str) -> Result<String, AgentResolveError> {
        debug!("AgentResolver::resolve: resolving agent_ref={:?}", agent_ref);
        if !agent_ref.contains(':') {
            return Err(AgentResolveError::InvalidReference(format!(
                "Agent reference must be in 'namespace:name' format, got: '{}'",
                agent_ref
            )));
        }

        let parts: Vec<&str> = agent_ref.split(':').collect();

        // Validate every segment to prevent path traversal
        for part in &parts {
            if !SAFE_NAME_RE.is_match(part) {
                return Err(AgentResolveError::InvalidReference(format!(
                    "Invalid agent reference segment '{}': must contain only \
                     alphanumeric characters, hyphens, and underscores",
                    part
                )));
            }
        }

        let (namespace, category, name) = match parts.len() {
            3 => (parts[0], Some(parts[1]), parts[2]),
            2 => (parts[0], None, parts[1]),
            _ => {
                return Err(AgentResolveError::InvalidReference(format!(
                    "Agent reference must be 'namespace:name' or \
                     'namespace:category:name', got: '{}'",
                    agent_ref
                )));
            }
        };

        // Build candidate paths
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(cat) = category {
            candidates.push(
                PathBuf::from(namespace)
                    .join(cat)
                    .join(format!("{}.md", name)),
            );
            candidates.push(PathBuf::from(cat).join(format!("{}.md", name)));
        }
        candidates.push(
            PathBuf::from(namespace)
                .join("core")
                .join(format!("{}.md", name)),
        );
        candidates.push(
            PathBuf::from(namespace)
                .join("specialized")
                .join(format!("{}.md", name)),
        );
        candidates.push(PathBuf::from("core").join(format!("{}.md", name)));
        candidates.push(PathBuf::from("specialized").join(format!("{}.md", name)));
        candidates.push(PathBuf::from(format!("{}.md", name)));

        let mut searched = Vec::new();
        for base in &self.search_paths {
            let resolved_base = match base.canonicalize() {
                Ok(p) => p,
                Err(_) => continue,
            };
            for candidate in &candidates {
                let full = base.join(candidate);
                searched.push(full.display().to_string());
                if full.is_file() {
                    // Defense in depth: verify resolved path is inside search directory
                    if let Ok(resolved_full) = full.canonicalize()
                        && resolved_full.starts_with(&resolved_base)
                    {
                        match std::fs::read_to_string(&full) {
                            Ok(content) => {
                                info!("AgentResolver::resolve: found agent '{}' at {:?}", agent_ref, full);
                                return Ok(content);
                            }
                            Err(_) => continue,
                        }
                    }
                }
            }
        }

        warn!("AgentResolver::resolve: agent '{}' not found (searched {} paths)", agent_ref, searched.len());
        Err(AgentResolveError::NotFound {
            agent_ref: agent_ref.to_string(),
            searched: format!(
                "{} director{} searched",
                searched.len(),
                if searched.len() == 1 { "y" } else { "ies" }
            ),
        })
    }
}

impl Default for AgentResolver {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reject_no_colon() {
        let resolver = AgentResolver::new(Some(vec![]));
        assert!(resolver.resolve("no_colon").is_err());
    }

    #[test]
    fn test_reject_path_traversal() {
        let resolver = AgentResolver::new(Some(vec![]));
        assert!(resolver.resolve("../etc:passwd").is_err());
    }

    #[test]
    fn test_reject_too_many_parts() {
        let resolver = AgentResolver::new(Some(vec![]));
        assert!(resolver.resolve("a:b:c:d").is_err());
    }

    #[test]
    fn test_valid_two_part_ref_not_found() {
        let resolver = AgentResolver::new(Some(vec![PathBuf::from("/nonexistent")]));
        let err = resolver.resolve("amplihack:builder").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_valid_three_part_ref_not_found() {
        let resolver = AgentResolver::new(Some(vec![PathBuf::from("/nonexistent")]));
        let err = resolver.resolve("amplihack:core:architect").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_resolve_from_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("amplihack").join("core");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("builder.md"), "You are a builder agent.").unwrap();

        let resolver = AgentResolver::new(Some(vec![tmp.path().to_path_buf()]));
        let content = resolver.resolve("amplihack:core:builder").unwrap();
        assert_eq!(content, "You are a builder agent.");
    }
}
