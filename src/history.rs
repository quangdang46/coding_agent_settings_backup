//! `casb history` implementation.
//!
//! Wraps `git log` for a given agent's backup repo. Returns parsed entries
//! with attached tags (if any), suitable for direct JSON/TOON serialisation.

use crate::agent::AgentConfig;
use crate::backup::require_repo;
use crate::config::Config;
use crate::error::Result;
use crate::git::LogEntry;

/// Output object for `casb history`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct History {
    /// Agent key.
    pub agent: String,
    /// Number of commits returned.
    pub count: usize,
    /// Commit entries, newest first.
    pub entries: Vec<LogEntry>,
}

/// Compute history for one agent.
pub fn agent_history(cfg: &Config, agent: &AgentConfig, limit: usize) -> Result<History> {
    let repo = require_repo(cfg)?;
    let entries = repo.log(limit)?;
    Ok(History {
        agent: agent.key.clone(),
        count: entries.len(),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentCategory, AgentConfig, AgentLocation, LocationKind, LocationType};
    use crate::backup::backup_agents;
    use tempfile::tempdir;

    fn cfg_for(root: &std::path::Path) -> Config {
        let mut c = Config::default();
        c.general.backup_root = root.display().to_string();
        c.backup.use_rsync = false;
        c
    }

    fn registry_with(agent: &AgentConfig) -> crate::agent::Registry {
        let mut cfg = Config::default();
        cfg.agents.insert(
            agent.key.clone(),
            crate::config::CustomAgentConfig {
                enabled: true,
                display_name: Some(agent.display_name.clone()),
                locations: agent
                    .locations
                    .iter()
                    .map(|l| l.path.display().to_string())
                    .collect(),
                exclusions: vec![],
            },
        );
        crate::agent::Registry::from_config(&cfg).unwrap()
    }

    #[test]
    fn history_returns_commits() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "1").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "h1".into(),
            display_name: "h1".into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".".into(),
            }],
        };
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["h1".into()], Some("c1"), false, false).unwrap();
        std::fs::write(src.path().join("a.txt"), "2").unwrap();
        backup_agents(&cfg, &reg, &["h1".into()], Some("c2"), false, false).unwrap();
        let resolved = reg.get("h1").unwrap();
        let h = agent_history(&cfg, resolved, 10).unwrap();
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[0].subject, "c2");
        assert_eq!(h.entries[1].subject, "c1");
    }
}
