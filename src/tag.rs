//! `casb tag` implementation.
//!
//! Thin wrappers around the git tag operations. The `restore-from-tag` path
//! reuses the existing [`crate::restore`] flow.

use crate::backup::require_repo;
use crate::config::Config;
use crate::error::Result;

/// Result entry for `casb tag list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TagEntry {
    /// Tag name.
    pub name: String,
    /// Commit hash the tag points to.
    pub commit: String,
}

/// Create a tag at HEAD with optional annotated message.
pub fn create_tag(
    cfg: &Config,
    _agent: &crate::agent::AgentConfig,
    name: &str,
    message: Option<&str>,
) -> Result<()> {
    let repo = require_repo(cfg)?;
    repo.tag_create(name, message)?;
    Ok(())
}

/// List tags with their target commit.
pub fn list_tags(cfg: &Config, _agent: &crate::agent::AgentConfig) -> Result<Vec<TagEntry>> {
    let repo = require_repo(cfg)?;
    let pairs = repo.tag_pairs()?;
    let mut entries: Vec<TagEntry> = pairs
        .into_iter()
        .map(|(name, commit)| TagEntry { name, commit })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Delete a tag.
pub fn delete_tag(cfg: &Config, _agent: &crate::agent::AgentConfig, name: &str) -> Result<()> {
    let repo = require_repo(cfg)?;
    repo.tag_delete(name)?;
    Ok(())
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
    fn create_list_delete_tag() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "x").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "t1".into(),
            display_name: "t1".into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".".into(),
            }],
        };
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["t1".into()], None, false, false).unwrap();
        let resolved = reg.get("t1").unwrap();

        create_tag(&cfg, resolved, "v1", Some("first")).unwrap();
        let tags = list_tags(&cfg, resolved).unwrap();
        assert!(tags.iter().any(|t| t.name == "v1"));
        delete_tag(&cfg, resolved, "v1").unwrap();
        let tags2 = list_tags(&cfg, resolved).unwrap();
        assert!(!tags2.iter().any(|t| t.name == "v1"));
    }
}
