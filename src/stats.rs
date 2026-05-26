//! `casb stats` implementation.
//!
//! Computes per-agent and aggregate statistics: commit count, repo size on
//! disk, number of installed agents, and total bytes.

use crate::agent::Registry;
use crate::config::Config;
use crate::error::Result;
use crate::git::Repo;
use crate::util::dir_size;

/// Per-agent stats line.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentStats {
    /// Agent key.
    pub agent: String,
    /// Whether the agent is installed on this machine.
    pub installed: bool,
    /// Whether a backup repo exists for it.
    pub repo_exists: bool,
    /// Number of commits in the backup repo.
    pub commits: u64,
    /// On-disk size of the backup repo (bytes).
    pub repo_bytes: u64,
    /// On-disk size of the live source location(s) (bytes).
    pub source_bytes: u64,
}

/// Aggregate stats across one or more agents.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Stats {
    /// Per-agent details.
    pub agents: Vec<AgentStats>,
    /// Total backup-repo size across all agents.
    pub total_repo_bytes: u64,
    /// Total live source size across all agents.
    pub total_source_bytes: u64,
    /// Total commit count.
    pub total_commits: u64,
}

/// Compute stats for one agent (if `key` is `Some`) or every known agent.
pub fn compute_stats(cfg: &Config, registry: &Registry, key: Option<&str>) -> Result<Stats> {
    let agents: Vec<&crate::agent::AgentConfig> = if let Some(k) = key {
        vec![registry.get(k)?]
    } else {
        registry.all().iter().collect()
    };

    let mut entries = Vec::new();
    let mut total_repo = 0u64;
    let mut total_source = 0u64;
    let mut total_commits = 0u64;

    // Shared repo stats (single .git at backup_root, same for all agents).
    let backup_root = cfg.backup_root();
    let repo = Repo::new(&backup_root);
    let repo_exists = repo.exists();
    let repo_commits = if repo_exists { repo.commit_count()? } else { 0 };
    let repo_bytes_total = if repo_exists { dir_size(&backup_root) } else { 0 };

    for agent in agents {
        let source_bytes: u64 = agent.installed_locations().map(|l| dir_size(&l.path)).sum();
        // Each agent's backup content lives in its subdirs; size is the whole
        // repo since all agents share one .git (shown as repo_bytes for each).
        total_repo += repo_bytes_total;
        total_source += source_bytes;
        total_commits += repo_commits;
        entries.push(AgentStats {
            agent: agent.key.clone(),
            installed: agent.is_installed(),
            repo_exists,
            commits: repo_commits,
            repo_bytes: repo_bytes_total,
            source_bytes,
        });
    }

    Ok(Stats {
        agents: entries,
        total_repo_bytes: total_repo,
        total_source_bytes: total_source,
        total_commits,
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

    fn registry_with(agent: &AgentConfig) -> Registry {
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
        Registry::from_config(&cfg).unwrap()
    }

    #[test]
    fn stats_for_one_agent() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "hello").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "s1".into(),
            display_name: "s1".into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".".into(),
            }],
        };
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["s1".into()], None, false, false).unwrap();
        let s = compute_stats(&cfg, &reg, Some("s1")).unwrap();
        assert_eq!(s.agents.len(), 1);
        assert_eq!(s.agents[0].agent, "s1");
        assert!(s.agents[0].repo_exists);
        assert!(s.agents[0].commits >= 1);
        assert!(s.total_repo_bytes > 0);
    }
}
