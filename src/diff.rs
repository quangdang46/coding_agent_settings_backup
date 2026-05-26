//! `casb diff` implementation.
//!
//! Compares the current state of an agent's source locations against the
//! latest committed snapshot in its backup repository. Reuses the
//! [`crate::restore::build_preview`] machinery against `HEAD`.

use crate::agent::AgentConfig;
use crate::backup::require_repo;
use crate::config::Config;
use crate::error::{CasbError, Result};
use crate::restore::{build_preview, RestorePreview};

/// Compute changes since the most recent backup commit.
pub fn diff_since_last_backup(cfg: &Config, agent: &AgentConfig) -> Result<RestorePreview> {
    let repo = require_repo(cfg)?;
    if !repo.ref_exists("HEAD") {
        return Err(CasbError::NoBackupRepo {
            key: agent.key.clone(),
        });
    }
    // Inverse of build_preview's perspective: we want changes the user has
    // made vs the backup. The same comparator works because it categorises
    // by presence on each side.
    let (preview, tmp) = build_preview(&repo, agent, "HEAD")?;
    repo.worktree_remove(tmp.path())?;
    // Swap added/removed: from the user's perspective, files in HEAD but
    // not in source are *removed* by the user; files in source but not HEAD
    // are *added* by the user.
    let mut user_view = RestorePreview {
        agent: preview.agent,
        added: preview.removed,
        removed: preview.added,
        modified: preview.modified,
    };
    user_view.added.sort();
    user_view.removed.sort();
    user_view.modified.sort();
    Ok(user_view)
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
    fn diff_detects_added_modified() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "v1").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "d1".into(),
            display_name: "d1".into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".".into(),
            }],
        };
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["d1".into()], None, false, false).unwrap();
        std::fs::write(src.path().join("a.txt"), "v2").unwrap();
        std::fs::write(src.path().join("new.txt"), "n").unwrap();
        let resolved = reg.get("d1").unwrap();
        let preview = diff_since_last_backup(&cfg, resolved).unwrap();
        assert!(preview.modified.iter().any(|p| p.contains("a.txt")));
        assert!(preview.added.iter().any(|p| p.contains("new.txt")));
    }

    #[test]
    fn diff_no_changes_is_empty() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "v1").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "d2".into(),
            display_name: "d2".into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".".into(),
            }],
        };
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["d2".into()], None, false, false).unwrap();
        let resolved = reg.get("d2").unwrap();
        let preview = diff_since_last_backup(&cfg, resolved).unwrap();
        assert_eq!(preview.total(), 0);
    }
}
