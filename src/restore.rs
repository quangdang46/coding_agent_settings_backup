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
use crate::agent::LocationKind;
use crate::backup::require_repo;
use crate::config::Config;
use crate::error::{CasbError, Result};
use crate::filter::ExclusionFilter;
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
///
/// `filter` must be the same exclusion filter that produced the backup.
/// Paths the backup deliberately omits are not user changes and must not
/// appear in the preview — nor may they be deleted when the restore runs.
pub fn build_preview(
    repo: &Repo,
    agent: &AgentConfig,
    reference: &str,
    filter: &ExclusionFilter,
) -> Result<(RestorePreview, tempfile::TempDir)> {
    if !repo.ref_exists(reference) {
        return Err(CasbError::InvalidArgument(format!(
            "ref does not resolve in {} repo: {reference}",
            agent.key
        )));
    }
    let tmp = tempfile::Builder::new().prefix("casb-restore-").tempdir()?;
    repo.worktree_add(tmp.path(), reference)?;

    // If preview building fails, the worktree must still be removed —
    // otherwise every failed restore leaks a registered worktree.
    let tmp_path = tmp.path().to_path_buf();
    let result = (|| -> Result<(RestorePreview, tempfile::TempDir)> {
        let mut preview = RestorePreview {
            agent: agent.key.clone(),
            ..Default::default()
        };

        for loc in &agent.locations {
            let backup_dir = backup_dest_for(tmp.path(), loc);
            let live = &loc.path;
            // A single-file location has no relative path of its own; use the
            // file name so the preview reports a meaningful path.
            let start = match loc.kind {
                LocationKind::File => live.file_name().map(PathBuf::from).unwrap_or_default(),
                LocationKind::Directory => PathBuf::new(),
            };
            compare_trees(&backup_dir, live, &start, filter, &mut preview)?;
        }
        Ok((preview, tmp))
    })();
    if result.is_err() {
        let _ = repo.worktree_remove(&tmp_path);
    }
    result
}

fn compare_trees(
    backup: &Path,
    live: &Path,
    rel: &Path,
    filter: &ExclusionFilter,
    preview: &mut RestorePreview,
) -> Result<()> {
    // Classify with symlink_metadata so symlinks are compared as links —
    // never followed. Following a symlinked directory would let the
    // comparison wander outside the agent tree.
    let backup_kind = entry_kind(backup);
    let live_kind = entry_kind(live);

    if backup_kind == EntryKind::Missing && live_kind == EntryKind::Missing {
        return Ok(());
    }

    // File-level comparison (files, symlinks, and type mismatches).
    if !matches!(backup_kind, EntryKind::Dir) || !matches!(live_kind, EntryKind::Dir) {
        let rel_str = rel.to_string_lossy().to_string();
        match (backup_kind, live_kind) {
            (EntryKind::Missing, _) => preview.removed.push(rel_str),
            (_, EntryKind::Missing) => preview.added.push(rel_str),
            (EntryKind::Dir, EntryKind::Dir) => unreachable!(),
            _ => {
                if entries_differ(backup, backup_kind, live, live_kind) {
                    preview.modified.push(rel_str);
                }
            }
        }
        return Ok(());
    }

    // Directory comparison.
    let mut names: std::collections::BTreeSet<std::ffi::OsString> = Default::default();
    for base in [backup, live] {
        if let Ok(rd) = std::fs::read_dir(base) {
            for entry in rd.flatten() {
                // Skip .git inside the backup tree to avoid noise.
                if entry.file_name() == ".git" {
                    continue;
                }
                names.insert(entry.file_name());
            }
        }
    }
    for name in names {
        if name == ".git" || name == ".gitignore" {
            continue;
        }
        let next_rel = rel.join(&name);
        // Skip paths the backup deliberately excludes. They are absent from
        // the backup by design, so reporting them as differences would fill
        // the preview with thousands of phantom changes.
        if filter.is_excluded(&next_rel) {
            continue;
        }
        compare_trees(
            &backup.join(&name),
            &live.join(&name),
            &next_rel,
            filter,
            preview,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Missing,
    Dir,
    File,
    Symlink,
    Other,
}

fn entry_kind(path: &Path) -> EntryKind {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => EntryKind::Symlink,
        Ok(m) if m.is_dir() => EntryKind::Dir,
        Ok(m) if m.is_file() => EntryKind::File,
        Ok(_) => EntryKind::Other,
        Err(_) => EntryKind::Missing,
    }
}

/// Do two existing entries differ? Symlinks compare by link target;
/// everything else compares by length first, then chunked content.
fn entries_differ(a: &Path, a_kind: EntryKind, b: &Path, b_kind: EntryKind) -> bool {
    if a_kind != b_kind {
        return true;
    }
    match a_kind {
        EntryKind::Symlink => std::fs::read_link(a).ok() != std::fs::read_link(b).ok(),
        EntryKind::File => files_differ(a, b),
        // Same non-file, non-symlink kind (e.g. fifo vs fifo): can't cheaply
        // compare, treat as equal to avoid phantom diffs.
        _ => false,
    }
}

/// Length-first file comparison: metadata check avoids reading two whole
/// files into memory when sizes already differ; equal sizes then compare
/// in 64 KiB chunks so large files never fully materialise either.
fn files_differ(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return true;
    };
    if ma.len() != mb.len() {
        return true;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return true;
    };
    use std::io::Read;
    let mut ba = [0u8; 65536];
    let mut bb = [0u8; 65536];
    loop {
        let na = match fa.read(&mut ba) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return true,
        };
        let nb = match fb.read(&mut bb) {
            Ok(n) => n,
            Err(_) => return true,
        };
        if na != nb || ba[..na] != bb[..nb] {
            return true;
        }
    }
    false
}

/// Execute a restore: build preview, prompt, then sync.
pub fn restore_agent(
    cfg: &Config,
    agent: &AgentConfig,
    reference: Option<&str>,
    force: bool,
    dry_run: bool,
) -> Result<(RestorePreview, RestoreOutcome)> {
    let repo = require_repo(cfg)?;
    let refname = reference.unwrap_or("HEAD").to_string();
    let backup_root = cfg.backup_root();
    let filter = crate::backup::build_agent_filter(cfg, agent, &backup_root)?;
    let (preview, tmp) = build_preview(&repo, agent, &refname, &filter)?;
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

    // The same filter that produced the backup is used here so that the
    // restore never deletes excluded paths from the live source directory.
    // worktree_remove runs on EVERY path — success or failure — so a
    // failed sync/hook never leaks a registered worktree.
    let sync_result = (|| -> Result<SyncStats> {
        run_hooks(HookKind::PreRestore, &agent.key)?;
        let stats =
            sync_backup_to_agent(agent, &target_path, &filter, cfg.backup.use_rsync, false)?;
        run_hooks(HookKind::PostRestore, &agent.key)?;
        Ok(stats)
    })();
    let remove_result = repo.worktree_remove(&target_path);

    match (sync_result, remove_result) {
        (Ok(stats), Ok(())) => {
            outcome.sync = stats;
            outcome.applied = true;
            Ok((preview, outcome))
        }
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
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

    /// Regression: a restore must never delete files that the backup
    /// deliberately excludes.
    ///
    /// `sessions/`, `cache/`, `history.jsonl` and friends are excluded from
    /// backups, so they are absent from the backup tree. A restore that
    /// syncs with an empty filter treats every absent file as "deleted"
    /// and wipes them from the live source directory.
    #[test]
    fn restore_does_not_delete_excluded_files() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("sessions")).unwrap();
        std::fs::create_dir_all(src.path().join("cache")).unwrap();
        std::fs::write(src.path().join("settings.json"), "{}").unwrap();
        std::fs::write(src.path().join("sessions/big.txt"), "IMPORTANT").unwrap();
        std::fs::write(src.path().join("cache/c.txt"), "cache").unwrap();

        let mut cfg = cfg_for(backup_root.path());
        cfg.backup.use_rsync = true; // exercise the rsync backend
        let agent = make_agent("re", src.path());
        let reg = registry_with(vec![agent.clone()]);
        let resolved = reg.get("re").unwrap();

        backup_agents(&cfg, &reg, &["re".into()], None, false, false).unwrap();
        assert!(
            !backup_root.path().join(".re/sessions/big.txt").exists(),
            "excluded file must not be in the backup"
        );

        let (_preview, outcome) = restore_agent(&cfg, resolved, None, true, false).unwrap();
        assert!(outcome.applied);
        assert!(
            src.path().join("sessions/big.txt").exists(),
            "restore must not delete excluded files from the source"
        );
        assert!(
            src.path().join("cache/c.txt").exists(),
            "restore must not delete excluded files from the source"
        );
        assert!(src.path().join("settings.json").exists());
    }

    /// Regression (bead custom-multilocation-data-loss-zwi): a custom agent
    /// with two locations must back up AND restore BOTH trees. Previously
    /// every location shared `.key`, so the second sync pruned the first —
    /// only the last location's data survived.
    #[test]
    fn multi_location_custom_agent_round_trips_all_locations() {
        let backup_root = tempdir().unwrap();
        let src_a = tempdir().unwrap();
        let src_b = tempdir().unwrap();
        std::fs::write(src_a.path().join("a.txt"), "AAA").unwrap();
        std::fs::create_dir_all(src_a.path().join("sub")).unwrap();
        std::fs::write(src_a.path().join("sub/a2.txt"), "A2").unwrap();
        std::fs::write(src_b.path().join("b.txt"), "BBB").unwrap();

        let cfg = cfg_for(backup_root.path());
        // Register through the config path so the real backup_subdir
        // assignment (per-location) is exercised, not a hand-built agent.
        let mut reg_cfg = Config::default();
        reg_cfg.agents.insert(
            "multi".to_string(),
            crate::config::CustomAgentConfig {
                enabled: true,
                display_name: Some("multi".into()),
                locations: vec![
                    src_a.path().display().to_string(),
                    src_b.path().display().to_string(),
                ],
                exclusions: vec![],
            },
        );
        let reg = crate::agent::Registry::from_config(&reg_cfg).unwrap();
        backup_agents(&cfg, &reg, &["multi".into()], None, false, false).unwrap();

        let subdirs: Vec<String> = reg
            .get("multi")
            .unwrap()
            .locations
            .iter()
            .map(|l| l.backup_subdir.clone())
            .collect();
        assert_ne!(subdirs[0], subdirs[1]);
        assert!(backup_root.path().join(&subdirs[0]).exists());
        assert!(backup_root.path().join(&subdirs[1]).exists());
        assert!(backup_root.path().join(&subdirs[0]).join("a.txt").exists());
        assert!(backup_root
            .path()
            .join(&subdirs[0])
            .join("sub/a2.txt")
            .exists());
        assert!(backup_root.path().join(&subdirs[1]).join("b.txt").exists());

        // Wipe the sources and restore: BOTH trees must come back.
        std::fs::remove_file(src_a.path().join("a.txt")).unwrap();
        std::fs::remove_file(src_b.path().join("b.txt")).unwrap();
        let resolved = reg.get("multi").unwrap();
        let (_preview, outcome) = restore_agent(&cfg, resolved, None, true, false).unwrap();
        assert!(outcome.applied);
        assert_eq!(
            std::fs::read_to_string(src_a.path().join("a.txt")).unwrap(),
            "AAA"
        );
        assert_eq!(
            std::fs::read_to_string(src_a.path().join("sub/a2.txt")).unwrap(),
            "A2"
        );
        assert_eq!(
            std::fs::read_to_string(src_b.path().join("b.txt")).unwrap(),
            "BBB"
        );
    }

    /// Regression: the restore preview must not list excluded paths as
    /// differences. Reporting thousands of intentionally-omitted files makes
    /// the preview useless.
    #[test]
    fn preview_ignores_excluded_paths() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("projects")).unwrap();
        std::fs::write(src.path().join("settings.json"), "{}").unwrap();
        std::fs::write(src.path().join("projects/big.txt"), "IMPORTANT").unwrap();

        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("rp", src.path());
        let reg = registry_with(vec![agent.clone()]);
        let resolved = reg.get("rp").unwrap();
        backup_agents(&cfg, &reg, &["rp".into()], None, false, false).unwrap();

        let (preview, _outcome) = restore_agent(&cfg, resolved, None, true, true).unwrap();
        assert_eq!(
            preview.total(),
            0,
            "excluded paths must not appear in the preview: {:?}",
            preview
        );
    }
}
