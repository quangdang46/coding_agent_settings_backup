//! `casb verify` implementation.
//!
//! Runs `git fsck` on each requested agent's backup repo and aggregates
//! warnings vs hard errors into a single report.

use crate::agent::Registry;
use crate::backup::agent_repo_path;
use crate::config::Config;
use crate::error::Result;
use crate::git::Repo;

/// Per-agent verification report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentVerifyReport {
    /// Agent key.
    pub agent: String,
    /// Whether the backup repo exists.
    pub repo_exists: bool,
    /// Whether `git fsck` reported success.
    pub ok: bool,
    /// Lines treated as hard errors.
    pub errors: Vec<String>,
    /// Lines treated as warnings.
    pub warnings: Vec<String>,
}

/// Aggregate verify report for many agents.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifyReport {
    /// Per-agent details.
    pub agents: Vec<AgentVerifyReport>,
    /// True when every agent reported `ok`.
    pub all_ok: bool,
}

/// Verify the supplied agent keys, or every installed/known agent if empty.
pub fn verify_agents(cfg: &Config, registry: &Registry, keys: &[String]) -> Result<VerifyReport> {
    let agents: Vec<&crate::agent::AgentConfig> = if keys.is_empty() {
        registry.all().iter().collect()
    } else {
        keys.iter()
            .map(|k| registry.get(k))
            .collect::<Result<Vec<_>>>()?
    };

    let mut reports = Vec::new();
    let mut all_ok = true;

    for agent in agents {
        let repo_path = agent_repo_path(&cfg.backup_root(), agent);
        let repo = Repo::new(&repo_path);
        if !repo.exists() {
            reports.push(AgentVerifyReport {
                agent: agent.key.clone(),
                repo_exists: false,
                ok: true, // not having a repo is not an error condition
                errors: vec![],
                warnings: vec![],
            });
            continue;
        }
        let result = repo.fsck()?;
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        for line in result.stderr.lines() {
            let lower = line.to_lowercase();
            if lower.contains("error") || lower.contains("missing") || lower.contains("dangling") {
                errors.push(line.to_string());
            } else if !line.trim().is_empty() {
                warnings.push(line.to_string());
            }
        }
        let ok = result.ok && errors.is_empty();
        if !ok {
            all_ok = false;
        }
        reports.push(AgentVerifyReport {
            agent: agent.key.clone(),
            repo_exists: true,
            ok,
            errors,
            warnings,
        });
    }

    Ok(VerifyReport {
        agents: reports,
        all_ok,
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
    fn verify_clean_repo_passes() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "x").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "v1".into(),
            display_name: "v1".into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".".into(),
            }],
        };
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["v1".into()], None, false, false).unwrap();
        let report = verify_agents(&cfg, &reg, &["v1".into()]).unwrap();
        assert!(report.all_ok);
        assert!(report.agents.iter().any(|r| r.agent == "v1" && r.ok));
    }

    #[test]
    fn verify_missing_repo_reports_no_repo() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "v2".into(),
            display_name: "v2".into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".".into(),
            }],
        };
        let reg = registry_with(&agent);
        let report = verify_agents(&cfg, &reg, &["v2".into()]).unwrap();
        let entry = report.agents.iter().find(|r| r.agent == "v2").unwrap();
        assert!(!entry.repo_exists);
    }
}
