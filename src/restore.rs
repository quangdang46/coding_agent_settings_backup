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

    // Directory comparison. A sorted Vec replaces the per-directory
    // BTreeSet — same ordering, fewer per-node allocations.
    let mut names: Vec<std::ffi::OsString> = Vec::new();
    for base in [backup, live] {
        if let Ok(rd) = std::fs::read_dir(base) {
            for entry in rd.flatten() {
                // Skip .git inside the backup tree to avoid noise.
                if entry.file_name() == ".git" {
                    continue;
                }
                names.push(entry.file_name());
            }
        }
    }
    names.sort();
    names.dedup();
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

/// Build a preview by comparing the backup ref's git objects against the
/// live tree — WITHOUT materialising a worktree. The backup side comes
/// from `git ls-tree` metadata; live files that exist on both sides are
/// compared against their blob contents fetched in batched `cat-file`
/// calls. This avoids the full checkout plus the dual-tree content walk
/// that [`build_preview`] performs on every `casb diff`/`casb restore`.
pub(crate) fn build_preview_git(
    repo: &Repo,
    agent: &AgentConfig,
    reference: &str,
    filter: &ExclusionFilter,
) -> Result<RestorePreview> {
    if !repo.ref_exists(reference) {
        return Err(CasbError::InvalidArgument(format!(
            "ref does not resolve in {} repo: {reference}",
            agent.key
        )));
    }
    let mut preview = RestorePreview {
        agent: agent.key.clone(),
        ..Default::default()
    };
    for loc in &agent.locations {
        preview_location_git(repo, loc, reference, filter, &mut preview)?;
    }
    Ok(preview)
}

/// One pending content comparison between a live path and a backup blob.
enum PendingCompare {
    /// Regular file vs blob.
    File {
        key: String,
        live: PathBuf,
        sha: String,
    },
    /// Symlink vs blob (blob content is the link target text).
    Link {
        key: String,
        live: PathBuf,
        sha: String,
    },
}

fn preview_location_git(
    repo: &Repo,
    loc: &crate::agent::AgentLocation,
    reference: &str,
    filter: &ExclusionFilter,
    preview: &mut RestorePreview,
) -> Result<()> {
    let sub = loc.backup_subdir.trim_end_matches('/');
    // `git ls-tree` pathspec: "." covers a root-level subdir.
    let pathspec = if sub.is_empty() { "." } else { sub };
    // Repo-relative prefix stripped from ls-tree paths to recover the
    // location-relative key `compare_trees` would have reported.
    let prefix = if sub.is_empty() || sub == "." {
        String::new()
    } else {
        format!("{sub}/")
    };

    // Backup side: rel-path -> (kind, blob sha). Tree entries give
    // directory structure; mode 120000 marks symlinks, 160000 gitlinks.
    let mut backup: std::collections::BTreeMap<String, (EntryKind, String)> =
        std::collections::BTreeMap::new();
    for e in repo.ls_tree(reference, &[pathspec.to_string()])? {
        let rel = if prefix.is_empty() {
            e.path.as_str()
        } else {
            match e.path.strip_prefix(prefix.as_str()) {
                Some(r) if !r.is_empty() => r,
                _ => continue,
            }
        };
        // Same skip rules compare_trees applies to every level: .git /
        // .gitignore names and filter-excluded paths never produce diff
        // entries.
        if rel.split('/').any(|c| c == ".git" || c == ".gitignore") {
            continue;
        }
        if filter.is_excluded(Path::new(rel)) {
            continue;
        }
        let kind = match e.kind.as_str() {
            "tree" => EntryKind::Dir,
            "commit" => EntryKind::Other,
            _ => {
                if e.mode == "120000" {
                    EntryKind::Symlink
                } else {
                    EntryKind::File
                }
            }
        };
        backup.insert(rel.to_string(), (kind, e.sha));
    }

    // Live side: rel-path -> (kind, absolute path for content reads).
    let mut live: std::collections::BTreeMap<String, (EntryKind, PathBuf)> =
        std::collections::BTreeMap::new();
    match loc.kind {
        LocationKind::File => {
            // Single-file location: the reported rel path is the basename,
            // exactly like compare_trees' `start` path — no exclusion or
            // name filtering applies at this top level.
            if let Ok(m) = std::fs::symlink_metadata(&loc.path) {
                let ft = m.file_type();
                let kind = if ft.is_symlink() {
                    EntryKind::Symlink
                } else if ft.is_dir() {
                    EntryKind::Dir
                } else if ft.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                };
                if let Some(name) = loc.path.file_name() {
                    live.insert(
                        name.to_string_lossy().replace('\\', "/"),
                        (kind, loc.path.clone()),
                    );
                }
            }
        }
        LocationKind::Directory => {
            if !crate::util::lexists(&loc.path) {
                // Live root missing entirely: compare_trees reports a
                // single empty-path entry rather than every file.
                if !backup.is_empty() {
                    preview.added.push(String::new());
                }
                return Ok(());
            }
            for entry in walkdir::WalkDir::new(&loc.path)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    if e.depth() == 0 {
                        return true;
                    }
                    let name = e.file_name();
                    if name == ".git" || name == ".gitignore" {
                        return false;
                    }
                    if e.file_type().is_dir() {
                        if let Ok(rel) = e.path().strip_prefix(&loc.path) {
                            return !filter.is_excluded(rel);
                        }
                    }
                    true
                })
                .filter_map(|e| e.ok())
            {
                let Ok(relp) = entry.path().strip_prefix(&loc.path) else {
                    continue;
                };
                if relp.as_os_str().is_empty() {
                    continue;
                }
                let rel = relp.to_string_lossy().replace('\\', "/");
                if rel.split('/').any(|c| c == ".git" || c == ".gitignore") {
                    continue;
                }
                if filter.is_excluded(Path::new(&rel)) {
                    continue;
                }
                let ft = entry.file_type();
                let kind = if ft.is_symlink() {
                    EntryKind::Symlink
                } else if ft.is_dir() {
                    EntryKind::Dir
                } else if ft.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                };
                live.insert(rel, (kind, entry.path().to_path_buf()));
            }
        }
    }

    // Union pass in sorted order, mirroring compare_trees' reporting:
    // a dir present on only one side (or kind-mismatched against a dir)
    // is reported once and its descendants are skipped.
    let mut pending: Vec<PendingCompare> = Vec::new();
    let mut skip_prefix: Option<String> = None;
    let keys: std::collections::BTreeSet<&String> = backup.keys().chain(live.keys()).collect();
    for key in keys {
        if let Some(sp) = &skip_prefix {
            if key.starts_with(sp.as_str()) {
                continue;
            }
            skip_prefix = None;
        }
        let bk = backup.get(key).map(|e| e.0);
        let lk = live.get(key).map(|e| e.0);
        match (bk, lk) {
            (Some(EntryKind::Dir), Some(EntryKind::Dir)) => {}
            (None, Some(EntryKind::Dir)) => {
                preview.removed.push(key.clone());
                skip_prefix = Some(format!("{key}/"));
            }
            (Some(EntryKind::Dir), None) => {
                preview.added.push(key.clone());
                skip_prefix = Some(format!("{key}/"));
            }
            (Some(EntryKind::Dir), Some(_)) | (Some(_), Some(EntryKind::Dir)) => {
                preview.modified.push(key.clone());
                skip_prefix = Some(format!("{key}/"));
            }
            (None, Some(_)) => preview.removed.push(key.clone()),
            (Some(_), None) => preview.added.push(key.clone()),
            (Some(EntryKind::File), Some(EntryKind::File)) => pending.push(PendingCompare::File {
                key: key.clone(),
                live: live[key].1.clone(),
                sha: backup[key].1.clone(),
            }),
            (Some(EntryKind::Symlink), Some(EntryKind::Symlink)) => {
                pending.push(PendingCompare::Link {
                    key: key.clone(),
                    live: live[key].1.clone(),
                    sha: backup[key].1.clone(),
                });
            }
            (Some(EntryKind::Other), Some(EntryKind::Other)) => {}
            _ => preview.modified.push(key.clone()),
        }
    }

    // Fetch blob contents in bounded chunks and compare. An unreadable
    // live file or missing blob counts as different — same policy as
    // `files_differ` returning true on error.
    const CHUNK: usize = 64;
    for chunk in pending.chunks(CHUNK) {
        let shas: Vec<String> = chunk
            .iter()
            .map(|w| match w {
                PendingCompare::File { sha, .. } | PendingCompare::Link { sha, .. } => sha.clone(),
            })
            .collect();
        let blobs = repo.cat_file_batch(&shas)?;
        for (work, blob) in chunk.iter().zip(blobs) {
            let (key, differs) = match (work, &blob) {
                (PendingCompare::File { key, live, .. }, Some(b)) => (key, !file_eq_blob(live, b)),
                (PendingCompare::Link { key, live, .. }, Some(b)) => (key, !link_eq_blob(live, b)),
                (PendingCompare::File { key, .. }, None)
                | (PendingCompare::Link { key, .. }, None) => (key, true),
            };
            if differs {
                preview.modified.push(key.clone());
            }
        }
    }
    Ok(())
}

/// Streaming equality between a live file and an in-memory blob —
/// length-first, then 64 KiB chunks, mirroring `files_differ`.
fn file_eq_blob(path: &Path, blob: &[u8]) -> bool {
    use std::io::Read;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() != blob.len() as u64 {
        return false;
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut off = 0usize;
    let mut buf = [0u8; 65536];
    while off < blob.len() {
        let want = (blob.len() - off).min(buf.len());
        match f.read(&mut buf[..want]) {
            Ok(0) => return false,
            Ok(n) => {
                if buf[..n] != blob[off..off + n] {
                    return false;
                }
                off += n;
            }
            Err(_) => return false,
        }
    }
    true
}

/// A live symlink equals its backup blob when the link target text
/// matches the blob content byte-for-byte.
fn link_eq_blob(path: &Path, blob: &[u8]) -> bool {
    std::fs::read_link(path)
        .map(|t| t.as_os_str().as_encoded_bytes() == blob)
        .unwrap_or(false)
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
    // Guard before previewing: the repo is shared, so `HEAD` resolves even
    // when this agent has never been backed up. Without this check the
    // preview lists every live file as "removed" — and an applied restore
    // would delete them all.
    let subdirs = crate::backup::agent_backup_subdirs(agent);
    if repo.commit_count_for_paths(&subdirs)? == 0 {
        return Err(CasbError::NoBackupRepo {
            key: agent.key.clone(),
        });
    }
    let refname = reference.unwrap_or("HEAD").to_string();
    let backup_root = cfg.backup_root();
    let filter = crate::backup::build_agent_filter(cfg, agent, &backup_root)?;
    // The preview runs against git objects directly — no worktree is
    // materialised unless the restore is actually applied.
    let preview = build_preview_git(&repo, agent, &refname, &filter)?;

    let mut outcome = RestoreOutcome {
        agent: agent.key.clone(),
        reference: refname.clone(),
        sync: SyncStats::default(),
        applied: false,
    };

    if dry_run {
        return Ok((preview, outcome));
    }

    if preview.total() == 0 {
        outcome.applied = true; // No-op restore is technically successful.
        return Ok((preview, outcome));
    }

    if !force && !confirm("Apply restore?", false)? {
        return Err(CasbError::Cancelled);
    }

    // Materialise the ref only now — the sync source must be real files.
    let tmp = tempfile::Builder::new().prefix("casb-restore-").tempdir()?;
    repo.worktree_add(tmp.path(), &refname)?;
    let target_path: PathBuf = tmp.path().to_path_buf();

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
