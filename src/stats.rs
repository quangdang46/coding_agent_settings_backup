//! `casb stats` implementation.
//!
//! Computes per-agent and aggregate statistics: commit count, repo size on
//! disk, number of installed agents, and total bytes.

use crate::agent::Registry;
use crate::config::Config;
use crate::error::Result;
use crate::git::Repo;
use crate::util::dir_size;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

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
    /// Commit count of the shared backup repository.
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
    let mut total_source = 0u64;

    // Shared repo stats (single .git at backup_root, same for all agents).
    let backup_root = cfg.backup_root();
    let repo = Repo::new(&backup_root);
    let repo_exists = repo.exists();
    let repo_commits = if repo_exists { repo.commit_count()? } else { 0 };

    // One walk over the backup tree yields BOTH the total size and every
    // location-subdir size — previously the root was walked once for the
    // total and then every agent subdir (and every live location) was
    // walked again on top of that.
    let needed_subdirs = needed_backup_subdirs(&agents);
    let (total_repo_bytes, subdir_sizes) = if repo_exists {
        backup_tree_sizes(&backup_root, &needed_subdirs)
    } else {
        (0, HashMap::new())
    };

    // Per-agent work (live-tree size + per-agent git history) is
    // independent — run it on the rayon pool instead of ~2 serial passes
    // plus one `git rev-list` subprocess per agent.
    let per_agent: Vec<Result<(u64, u64, u64)>> = agents
        .par_iter()
        .map(|agent| {
            let locs: Vec<_> = agent.installed_locations().collect();
            let source_bytes: u64 = locs.iter().map(|l| dir_size(&l.path)).sum();
            // Per-agent: size of its data subdir(s) inside the backup root.
            let agent_repo_bytes: u64 = locs
                .iter()
                .map(|l| {
                    let sub = l.backup_subdir.trim_end_matches('/');
                    if sub.is_empty() || sub == "." {
                        // A "." subdir means the whole backup root — same
                        // as dir_size(backup_root.join(".")) did before.
                        total_repo_bytes
                    } else {
                        subdir_sizes.get(sub).copied().unwrap_or(0)
                    }
                })
                .sum();
            // Commits that actually touched this agent's directories. The
            // shared repo's total commit count is not this agent's count.
            let agent_commits = if repo_exists {
                repo.commit_count_for_paths(&[agent.backup_root_subdir()])?
            } else {
                0
            };
            Ok((source_bytes, agent_repo_bytes, agent_commits))
        })
        .collect();
    let per_agent = per_agent.into_iter().collect::<Result<Vec<_>>>()?;

    for (agent, (source_bytes, agent_repo_bytes, agent_commits)) in
        agents.iter().zip(per_agent.iter())
    {
        total_source += source_bytes;
        entries.push(AgentStats {
            agent: agent.key.clone(),
            installed: agent.is_installed(),
            repo_exists,
            commits: *agent_commits,
            repo_bytes: *agent_repo_bytes,
            source_bytes: *source_bytes,
        });
    }

    Ok(Stats {
        agents: entries,
        total_repo_bytes,
        total_source_bytes: total_source,
        // The repository holds one shared history, so the total is the
        // repository's commit count — counted once, not summed per agent.
        total_commits: repo_commits,
    })
}

/// The set of location `backup_subdir`s that per-agent repo sizes are
/// keyed by, plus whether any location maps to the repo root itself.
fn needed_backup_subdirs(agents: &[&crate::agent::AgentConfig]) -> HashSet<String> {
    agents
        .iter()
        .flat_map(|a| a.installed_locations())
        .map(|l| l.backup_subdir.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty() && s != ".")
        .collect()
}

/// Walk `root` once, returning (total bytes, per-subdir bytes). Only
/// regular files count — symlinks are not followed, matching [`dir_size`].
fn backup_tree_sizes(root: &Path, subdirs: &HashSet<String>) -> (u64, HashMap<String, u64>) {
    let mut total = 0u64;
    let mut sizes: HashMap<String, u64> = subdirs.iter().map(|s| (s.clone(), 0)).collect();
    let mut prefix = String::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        total += len;
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        // Charge the file to every ancestor directory that is a known
        // location subdir (`.claude/home`, …). Component-wise prefix
        // building keeps separators normalised to '/'.
        prefix.clear();
        for comp in rel.components() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(&comp.as_os_str().to_string_lossy());
            if let Some(v) = sizes.get_mut(prefix.as_str()) {
                *v += len;
            }
        }
    }
    (total, sizes)
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
