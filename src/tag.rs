//! `casb tag` implementation.
//!
//! Git tags live in a single repo-global namespace, but every agent shares
//! that one backup repository — an unscoped `casb tag create claude v1`
//! would collide with (or silently retarget) another agent's tag, and
//! `git tag` always points at the shared HEAD, which may be a *different*
//! agent's most recent commit.
//!
//! Tags are therefore namespaced as `<agent-key>/<name>` and always created
//! at the agent's own most recent commit. Users still refer to tags by the
//! bare name; the agent prefix is added and stripped internally. Passing an
//! already-prefixed name is accepted and not double-prefixed.
//!
//! The `restore-from-tag` path reuses the existing [`crate::restore`] flow.

use crate::backup::require_repo;
use crate::config::Config;
use crate::error::{CasbError, Result};

/// Result entry for `casb tag list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TagEntry {
    /// Tag name (agent prefix stripped for display).
    pub name: String,
    /// Commit hash the tag points to.
    pub commit: String,
}

/// The on-disk (git) tag name for a user-supplied `name` under `agent_key`.
pub fn scoped_name(agent_key: &str, name: &str) -> String {
    let prefix = format!("{agent_key}/");
    if name.starts_with(&prefix) {
        name.to_string()
    } else {
        format!("{prefix}{name}")
    }
}

/// Strip the agent prefix for display; names without it pass through.
fn display_name(agent_key: &str, stored: &str) -> String {
    stored
        .strip_prefix(&format!("{agent_key}/"))
        .unwrap_or(stored)
        .to_string()
}

/// Create a tag at the agent's most recent backup commit.
pub fn create_tag(
    cfg: &Config,
    agent: &crate::agent::AgentConfig,
    name: &str,
    message: Option<&str>,
) -> Result<()> {
    let repo = require_repo(cfg)?;
    // Point at the agent's own last commit, never the shared repo HEAD —
    // HEAD may belong to a different agent entirely.
    let subdirs = crate::backup::agent_backup_subdirs(agent);
    let target = repo.last_commit_for_paths(&subdirs)?.ok_or_else(|| {
        CasbError::InvalidArgument(format!("agent {} has no backup commits to tag", agent.key))
    })?;
    repo.tag_create_at(&scoped_name(&agent.key, name), message, &target)?;
    Ok(())
}

/// List tags belonging to `agent`, with the agent prefix stripped.
pub fn list_tags(cfg: &Config, agent: &crate::agent::AgentConfig) -> Result<Vec<TagEntry>> {
    let repo = require_repo(cfg)?;
    let prefix = format!("{}/", agent.key);
    let pairs = repo.tag_pairs()?;
    let mut entries: Vec<TagEntry> = pairs
        .into_iter()
        .filter(|(name, _)| name.starts_with(&prefix))
        .map(|(name, commit)| TagEntry {
            name: display_name(&agent.key, &name),
            commit,
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Delete a tag belonging to `agent`.
pub fn delete_tag(cfg: &Config, agent: &crate::agent::AgentConfig, name: &str) -> Result<()> {
    let repo = require_repo(cfg)?;
    repo.tag_delete(&scoped_name(&agent.key, name))?;
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

    fn agent(key: &str, src: &std::path::Path) -> AgentConfig {
        AgentConfig {
            key: key.into(),
            display_name: key.into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: format!(".{key}"),
            }],
        }
    }

    #[test]
    fn create_list_delete_tag() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "x").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = agent("t1", src.path());
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

    /// Regression: tags are repo-global in git. `casb tag create` for one
    /// agent must not create a tag visible to (or pointing at the HEAD of)
    /// another agent.
    #[test]
    fn tags_are_scoped_per_agent_and_point_at_agent_commit() {
        let backup_root = tempdir().unwrap();
        let src_a = tempdir().unwrap();
        let src_b = tempdir().unwrap();
        std::fs::write(src_a.path().join("a.txt"), "a").unwrap();
        std::fs::write(src_b.path().join("b.txt"), "b").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent_a = agent("ta", src_a.path());
        let agent_b = agent("tb", src_b.path());
        // Register both agents in one registry.
        let mut reg_cfg = Config::default();
        for a in [&agent_a, &agent_b] {
            reg_cfg.agents.insert(
                a.key.clone(),
                crate::config::CustomAgentConfig {
                    enabled: true,
                    display_name: Some(a.display_name.clone()),
                    locations: a
                        .locations
                        .iter()
                        .map(|l| l.path.display().to_string())
                        .collect(),
                    exclusions: vec![],
                },
            );
        }
        let reg = crate::agent::Registry::from_config(&reg_cfg).unwrap();
        backup_agents(&cfg, &reg, &["ta".into(), "tb".into()], None, false, false).unwrap();

        let ra = reg.get("ta").unwrap();
        let rb = reg.get("tb").unwrap();
        create_tag(&cfg, ra, "v1", None).unwrap();

        // Agent A sees the tag; agent B does not.
        let tags_a = list_tags(&cfg, ra).unwrap();
        assert!(tags_a.iter().any(|t| t.name == "v1"));
        let tags_b = list_tags(&cfg, rb).unwrap();
        assert!(tags_b.is_empty(), "tags must not leak across agents");

        // The stored git tag is namespaced and points at A's commit, not
        // the shared HEAD (which is B's commit — backed up last).
        let repo = crate::git::Repo::new(cfg.backup_root());
        let pairs = repo.tag_pairs().unwrap();
        let (_, target) = pairs
            .iter()
            .find(|(n, _)| n == "ta/v1")
            .expect("tag must be stored as ta/v1");
        let a_commit = repo
            .last_commit_for_paths(&crate::backup::agent_backup_subdirs(ra))
            .unwrap()
            .unwrap();
        // `tag_pairs` reports full hashes; `last_commit_for_paths` short.
        assert!(
            target.starts_with(&a_commit),
            "tag {target} must point at agent A's commit {a_commit}"
        );
        let head_full = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .current_dir(cfg.backup_root())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        assert_ne!(
            target, &head_full,
            "tag must not point at the shared repo HEAD"
        );

        // Delete via the bare name still works.
        delete_tag(&cfg, ra, "v1").unwrap();
        assert!(list_tags(&cfg, ra).unwrap().is_empty());
    }

    #[test]
    fn tag_create_without_commits_errors() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "x").unwrap();
        let cfg = cfg_for(backup_root.path());
        // Init an empty repo but never back up.
        crate::git::Repo::new(cfg.backup_root()).init().unwrap();
        let agent = agent("tn", src.path());
        let reg = registry_with(&agent);
        let resolved = reg.get("tn").unwrap();
        assert!(create_tag(&cfg, resolved, "v1", None).is_err());
    }
}
