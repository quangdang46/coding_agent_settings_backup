//! `casb restore` implementation.
//!
//! Workflow:
//! 1. Resolve the agent and its backup repo.
//! 2. If a `reference` is supplied, verify it exists; otherwise use HEAD.
//! 3. Materialise that ref into a temporary worktree.
//! 4. Show a preview of changes (added/removed/modified) vs current source.
//! 5. Confirm (unless `--force`).
//! 6. Sync from the temp worktree back into the agent's source locations.

use crate::agent::AgentConfig;
use crate::backup::require_repo;
use crate::config::Config;
use crate::error::{CasbError, Result};
use crate::git::Repo;
use crate::hooks::{run_hooks, HookKind};
use crate::sync::{backup_dest_for, sync_backup_to_agent, SyncStats};
use crate::util::confirm;
use std::path::{Path, PathBuf};

/// Outcome of a restore operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RestoreOutcome {
    /// Agent key.
    pub agent: String,
    /// Reference that was restored.
    pub reference: String,
    /// Sync stats from copying back into the source.
    pub sync: SyncStats,
    /// Whether the user confirmed and the restore actually executed.
    pub applied: bool,
}

/// Compute the preview lists for a restore (added / removed / modified
/// relative paths). Backup contents at `reference` vs. current source.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RestorePreview {
    /// Agent key being previewed.
    pub agent: String,
    /// Files that exist in the backup but not in the source.
    pub added: Vec<String>,
    /// Files that exist in the source but not in the backup.
    pub removed: Vec<String>,
    /// Files that differ in content.
    pub modified: Vec<String>,
}

impl RestorePreview {
    /// Total number of differing files.
    pub fn total(&self) -> usize {
        self.added.len() + self.removed.len() + self.modified.len()
    }
}

/// Materialise the supplied ref into a temporary directory and produce a
/// preview of changes that would be applied. The temp directory is returned
/// alongside the preview so the caller can sync from it.
pub fn build_preview(
    repo: &Repo,
    agent: &AgentConfig,
    reference: &str,
) -> Result<(RestorePreview, tempfile::TempDir)> {
    if !repo.ref_exists(reference) {
        return Err(CasbError::InvalidArgument(format!(
            "ref does not resolve in {} repo: {reference}",
            agent.key
        )));
    }
    let tmp = tempfile::Builder::new().prefix("casb-restore-").tempdir()?;
    repo.worktree_add(tmp.path(), reference)?;

    let mut preview = RestorePreview {
        agent: agent.key.clone(),
        ..Default::default()
    };

    for loc in &agent.locations {
        let backup_dir = backup_dest_for(tmp.path(), loc);
        let live = &loc.path;
        compare_trees(&backup_dir, live, Path::new(""), &mut preview)?;
    }
    Ok((preview, tmp))
}

fn compare_trees(
    backup: &Path,
    live: &Path,
    rel: &Path,
    preview: &mut RestorePreview,
) -> Result<()> {
    let backup_exists = backup.exists();
    let live_exists = live.exists();
    if !backup_exists && !live_exists {
        return Ok(());
    }

    if backup.is_file() || live.is_file() {
        let rel_str = rel.to_string_lossy().to_string();
        if backup_exists && !live_exists {
            preview.added.push(rel_str);
        } else if !backup_exists && live_exists {
            preview.removed.push(rel_str);
        } else {
            // Compare bytes.
            let a = std::fs::read(backup).unwrap_or_default();
            let b = std::fs::read(live).unwrap_or_default();
            if a != b {
                preview.modified.push(rel_str);
            }
        }
        return Ok(());
    }

    // Directory comparison.
    let mut names: std::collections::BTreeSet<std::ffi::OsString> = Default::default();
    if backup_exists {
        for entry in std::fs::read_dir(backup)? {
            let entry = entry?;
            names.insert(entry.file_name());
        }
    }
    if live_exists {
        for entry in std::fs::read_dir(live)? {
            let entry = entry?;
            // Skip .git inside the backup tree to avoid noise.
            if entry.file_name() == ".git" {
                continue;
            }
            names.insert(entry.file_name());
        }
    }
    for name in names {
        if name == ".git" || name == ".gitignore" {
            continue;
        }
        let next_rel = rel.join(&name);
        compare_trees(&backup.join(&name), &live.join(&name), &next_rel, preview)?;
    }
    Ok(())
}

/// Execute a restore: build preview, prompt, then sync.
pub fn restore_agent(
    cfg: &Config,
    agent: &AgentConfig,
    reference: Option<&str>,
    force: bool,
    dry_run: bool,
) -> Result<(RestorePreview, RestoreOutcome)> {
    let repo = require_repo(cfg, agent)?;
    let refname = reference.unwrap_or("HEAD").to_string();
    let (preview, tmp) = build_preview(&repo, agent, &refname)?;
    let target_path: PathBuf = tmp.path().to_path_buf();

    let mut outcome = RestoreOutcome {
        agent: agent.key.clone(),
        reference: refname.clone(),
        sync: SyncStats::default(),
        applied: false,
    };

    if dry_run {
        // Make sure to clean up the worktree we added.
        repo.worktree_remove(&target_path)?;
        return Ok((preview, outcome));
    }

    if preview.total() == 0 {
        repo.worktree_remove(&target_path)?;
        outcome.applied = true; // No-op restore is technically successful.
        return Ok((preview, outcome));
    }

    if !force && !confirm("Apply restore?", false)? {
        repo.worktree_remove(&target_path)?;
        return Err(CasbError::Cancelled);
    }

    // Pre-restore hooks.
    run_hooks(HookKind::PreRestore, &agent.key)?;
    let stats = sync_backup_to_agent(agent, &target_path, cfg.backup.use_rsync, false)?;
    run_hooks(HookKind::PostRestore, &agent.key)?;
    repo.worktree_remove(&target_path)?;

    outcome.sync = stats;
    outcome.applied = true;
    Ok((preview, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentCategory, AgentLocation, LocationKind, LocationType};
    use crate::backup::backup_agents;
    use crate::config::Config;
    use tempfile::tempdir;

    fn cfg_for(root: &std::path::Path) -> Config {
        let mut c = Config::default();
        c.general.backup_root = root.display().to_string();
        c.backup.use_rsync = false;
        c
    }

    fn make_agent(key: &str, src: &std::path::Path) -> AgentConfig {
        AgentConfig {
            key: key.into(),
            display_name: key.into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".".into(),
            }],
        }
    }

    fn registry_with(agents: Vec<AgentConfig>) -> crate::agent::Registry {
        let mut cfg = Config::default();
        for a in &agents {
            cfg.agents.insert(
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
        crate::agent::Registry::from_config(&cfg).unwrap()
    }

    #[test]
    fn restore_no_changes_is_noop() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "x").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("ra", src.path());
        let reg = registry_with(vec![agent.clone()]);
        let resolved = reg.get("ra").unwrap();
        backup_agents(&cfg, &reg, &["ra".into()], None, false, false).unwrap();
        let (preview, outcome) = restore_agent(&cfg, resolved, None, true, false).unwrap();
        assert_eq!(preview.total(), 0);
        assert!(outcome.applied);
    }

    #[test]
    fn restore_applies_changes_with_force() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "v1").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("rb", src.path());
        let reg = registry_with(vec![agent.clone()]);
        let resolved = reg.get("rb").unwrap();
        backup_agents(&cfg, &reg, &["rb".into()], None, false, false).unwrap();
        // Modify source.
        std::fs::write(src.path().join("a.txt"), "v2-modified").unwrap();
        std::fs::write(src.path().join("new.txt"), "added").unwrap();
        let (preview, outcome) = restore_agent(&cfg, resolved, None, true, false).unwrap();
        assert!(preview.total() >= 1);
        assert!(outcome.applied);
        let restored = std::fs::read_to_string(src.path().join("a.txt")).unwrap();
        assert_eq!(restored, "v1");
    }

    #[test]
    fn restore_dry_run_does_not_apply() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "v1").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("rc", src.path());
        let reg = registry_with(vec![agent.clone()]);
        let resolved = reg.get("rc").unwrap();
        backup_agents(&cfg, &reg, &["rc".into()], None, false, false).unwrap();
        std::fs::write(src.path().join("a.txt"), "v2").unwrap();
        let (preview, outcome) = restore_agent(&cfg, resolved, None, true, true).unwrap();
        assert!(preview.total() >= 1);
        assert!(!outcome.applied);
        let after = std::fs::read_to_string(src.path().join("a.txt")).unwrap();
        assert_eq!(after, "v2", "dry-run must not change source");
    }

    #[test]
    fn restore_unknown_ref_errors() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "x").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("rd", src.path());
        let reg = registry_with(vec![agent.clone()]);
        let resolved = reg.get("rd").unwrap();
        backup_agents(&cfg, &reg, &["rd".into()], None, false, false).unwrap();
        let err = restore_agent(&cfg, resolved, Some("does-not-exist"), true, false);
        assert!(err.is_err());
    }
}
