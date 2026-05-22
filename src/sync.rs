//! File synchronisation primitives.
//!
//! Implements two backends used by the backup/restore commands:
//! - `rsync`-based copy (when available) for performance and atomicity.
//! - A pure-Rust fallback using `walkdir` + `std::fs` when `rsync` is absent.
//!
//! Multi-location agents are handled by syncing each location into a
//! separate subdirectory of the destination repo so that paths never collide.

use crate::agent::{AgentConfig, AgentLocation, LocationKind};
use crate::error::{CasbError, Result};
use crate::filter::ExclusionFilter;
use crate::util;
use std::path::{Path, PathBuf};

/// Statistics returned by a sync operation.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SyncStats {
    /// Files copied during this sync.
    pub files_copied: usize,
    /// Files skipped due to exclusions.
    pub files_skipped: usize,
    /// Total bytes copied.
    pub bytes_copied: u64,
    /// Whether the rsync backend was used.
    pub used_rsync: bool,
}

impl SyncStats {
    /// Combine two `SyncStats`, summing each field.
    pub fn merge(&mut self, other: &Self) {
        self.files_copied += other.files_copied;
        self.files_skipped += other.files_skipped;
        self.bytes_copied += other.bytes_copied;
        self.used_rsync = self.used_rsync || other.used_rsync;
    }
}

/// Sync every existing location of `agent` into `dest_root`.
///
/// `dest_root` is the per-agent backup repository directory. Each location
/// is written to `dest_root/<backup_subdir>` (or directly to `dest_root` for
/// `backup_subdir == "."`).
pub fn sync_agent_to_backup(
    agent: &AgentConfig,
    dest_root: &Path,
    filter: &ExclusionFilter,
    use_rsync: bool,
    dry_run: bool,
) -> Result<SyncStats> {
    let mut total = SyncStats::default();
    if !dest_root.exists() && !dry_run {
        std::fs::create_dir_all(dest_root)?;
    }

    // Drop subdirs that no longer correspond to any installed location;
    // otherwise stale data would persist forever in the repo. Skip the
    // `.git` directory and the root `.gitignore` we create.
    if !dry_run {
        prune_stale_subdirs(agent, dest_root)?;
    }

    for loc in agent.installed_locations() {
        let dest = backup_dest_for(dest_root, loc);
        if !dry_run {
            // For File-kind locations, `dest` is the final file path -- we
            // must only ensure the parent directory exists, NOT create the
            // file itself as a directory (which would later fail with
            // EISDIR inside sync_file).
            match loc.kind {
                LocationKind::Directory => {
                    if !dest.exists() {
                        std::fs::create_dir_all(&dest)?;
                    }
                }
                LocationKind::File => {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                }
            }
        }
        let stats = sync_location(loc, &dest, filter, use_rsync, dry_run)?;
        total.merge(&stats);
    }
    Ok(total)
}

/// Sync `dest_root` (a backup repo) back into the agent's source locations.
///
/// Used by `casb restore`. For each installed/known location, copies files
/// from `dest_root/<backup_subdir>` over the source path.
pub fn sync_backup_to_agent(
    agent: &AgentConfig,
    src_root: &Path,
    use_rsync: bool,
    dry_run: bool,
) -> Result<SyncStats> {
    let mut total = SyncStats::default();
    let empty = ExclusionFilter::new(vec![]).expect("empty filter must compile");
    for loc in &agent.locations {
        let src = backup_dest_for(src_root, loc);
        if !src.exists() {
            continue;
        }
        if !loc.path.exists() && !dry_run {
            if matches!(loc.kind, LocationKind::Directory) {
                std::fs::create_dir_all(&loc.path)?;
            } else if let Some(parent) = loc.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let stats = sync_location_raw(&src, &loc.path, &empty, use_rsync, dry_run, loc.kind)?;
        total.merge(&stats);
    }
    Ok(total)
}

/// Compute the destination subpath for a single agent location.
pub fn backup_dest_for(dest_root: &Path, loc: &AgentLocation) -> PathBuf {
    if matches!(loc.kind, LocationKind::File) {
        // Files are stored under the subdir as their basename.
        let name = loc
            .path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("file"));
        if loc.backup_subdir == "." || loc.backup_subdir.is_empty() {
            dest_root.join(name)
        } else {
            dest_root.join(&loc.backup_subdir).join(name)
        }
    } else if loc.backup_subdir == "." || loc.backup_subdir.is_empty() {
        dest_root.to_path_buf()
    } else {
        dest_root.join(&loc.backup_subdir)
    }
}

fn prune_stale_subdirs(agent: &AgentConfig, dest_root: &Path) -> Result<()> {
    let known: std::collections::HashSet<String> = agent
        .locations
        .iter()
        .map(|l| {
            if matches!(l.kind, LocationKind::File) {
                // For root files we don't prune anything: they live as
                // basenames. The basename will be overwritten on copy.
                "<file>".to_string()
            } else if l.backup_subdir == "." || l.backup_subdir.is_empty() {
                "<root>".to_string()
            } else {
                l.backup_subdir.clone()
            }
        })
        .collect();
    if !dest_root.exists() {
        return Ok(());
    }

    // If any location uses the root, we cannot safely delete unknown root
    // entries because they may be valid agent files. The `.git` dir and the
    // top-level `.gitignore` we create are always preserved.
    for entry in std::fs::read_dir(dest_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if name_str == ".git" || name_str == ".gitignore" || name_str == ".casbignore" {
            continue;
        }
        if entry.file_type()?.is_dir() && !known.contains(&name_str) {
            // Only prune if no location uses '.' as backup_subdir AND the
            // directory name matches one of the previously known subdirs
            // ('home', 'data', 'config', 'root'). This is conservative.
            if known.contains("<root>") {
                continue;
            }
            if matches!(name_str.as_str(), "home" | "data" | "config" | "root") {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
    }
    Ok(())
}

fn sync_location(
    loc: &AgentLocation,
    dest: &Path,
    filter: &ExclusionFilter,
    use_rsync: bool,
    dry_run: bool,
) -> Result<SyncStats> {
    sync_location_raw(&loc.path, dest, filter, use_rsync, dry_run, loc.kind)
}

fn sync_location_raw(
    src: &Path,
    dest: &Path,
    filter: &ExclusionFilter,
    use_rsync: bool,
    dry_run: bool,
    kind: LocationKind,
) -> Result<SyncStats> {
    if !src.exists() {
        return Ok(SyncStats::default());
    }
    if matches!(kind, LocationKind::File) {
        return sync_file(src, dest, filter, dry_run);
    }
    if use_rsync && util::which("rsync") {
        return sync_rsync(src, dest, filter, dry_run);
    }
    sync_walkdir(src, dest, filter, dry_run)
}

fn sync_file(
    src: &Path,
    dest: &Path,
    filter: &ExclusionFilter,
    dry_run: bool,
) -> Result<SyncStats> {
    let basename = src.file_name().unwrap_or_default();
    if filter.is_excluded(Path::new(basename)) {
        return Ok(SyncStats {
            files_skipped: 1,
            ..Default::default()
        });
    }
    if dry_run {
        return Ok(SyncStats {
            files_copied: 1,
            ..Default::default()
        });
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = std::fs::copy(src, dest)?;
    Ok(SyncStats {
        files_copied: 1,
        bytes_copied: bytes,
        ..Default::default()
    })
}

fn sync_rsync(
    src: &Path,
    dest: &Path,
    filter: &ExclusionFilter,
    dry_run: bool,
) -> Result<SyncStats> {
    use std::process::Command;

    let mut args = vec!["-a".to_string(), "--delete".to_string()];
    if dry_run {
        args.push("--dry-run".to_string());
    }
    // Always protect the git metadata directory from --delete; otherwise
    // rsync will remove .git (it exists only in the destination, not the
    // source) and subsequent git operations will fail.
    args.push("--exclude".to_string());
    args.push(".git".to_string());
    for pat in filter.raw_patterns() {
        args.push("--exclude".to_string());
        args.push(pat.clone());
    }
    let src_arg = format!("{}/", src.display());
    let dest_arg = format!("{}/", dest.display());
    args.push(src_arg);
    args.push(dest_arg);

    let out = Command::new("rsync").args(&args).output()?;
    if !out.status.success() {
        return Err(CasbError::CommandFailed {
            command: format!("rsync {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    // rsync stats: count files copied via walkdir of dest (cheap relative to copy).
    let files_copied = walkdir::WalkDir::new(dest)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count();
    let bytes_copied = util::dir_size(dest);
    Ok(SyncStats {
        files_copied,
        bytes_copied,
        used_rsync: true,
        ..Default::default()
    })
}

fn sync_walkdir(
    src: &Path,
    dest: &Path,
    filter: &ExclusionFilter,
    dry_run: bool,
) -> Result<SyncStats> {
    let mut stats = SyncStats::default();
    if !dry_run {
        std::fs::create_dir_all(dest)?;
    }

    // Track all relative paths we copied; remove anything in dest not in this set.
    let mut kept: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for entry in walkdir::WalkDir::new(src)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Never traverse into a `.git` directory or copy a `.git` file
            // from the source. The destination owns the only legitimate
            // `.git` (the backup repo metadata); pulling one in from the
            // source produces gitlinks/EISDIR errors on the next backup.
            // Mirrors the `--exclude .git` we already pass to rsync.
            e.path() == src || e.file_name() != ".git"
        })
        .filter_map(|e| e.ok())
    {
        let rel = match entry.path().strip_prefix(src) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        if filter.is_excluded(&rel) {
            stats.files_skipped += 1;
            continue;
        }
        let dest_path = dest.join(&rel);
        kept.insert(rel.clone());
        if entry.file_type().is_dir() {
            if !dry_run {
                std::fs::create_dir_all(&dest_path)?;
            }
        } else if entry.file_type().is_file() {
            if dry_run {
                stats.files_copied += 1;
                continue;
            }
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = std::fs::copy(entry.path(), &dest_path)?;
            stats.files_copied += 1;
            stats.bytes_copied += bytes;
        }
    }

    if !dry_run && dest.exists() {
        prune_unknown(dest, dest, &kept)?;
    }
    Ok(stats)
}

/// Remove files/dirs under `root` that don't appear in `kept` (relative
/// paths). `.git` is always preserved.
fn prune_unknown(root: &Path, cur: &Path, kept: &std::collections::HashSet<PathBuf>) -> Result<()> {
    if !cur.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(cur)? {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if rel.as_os_str() == ".git" {
            continue;
        }
        let ftype = entry.file_type()?;
        if ftype.is_dir() {
            prune_unknown(root, &path, kept)?;
            // After pruning, remove if empty and not in kept.
            if !kept.contains(&rel) && std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
        } else if !kept.contains(&rel) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentLocation, LocationType};
    use tempfile::tempdir;

    fn make_loc(path: PathBuf, sub: &str) -> AgentLocation {
        AgentLocation {
            path,
            location_type: LocationType::HomeDir,
            kind: LocationKind::Directory,
            backup_subdir: sub.to_string(),
        }
    }

    #[test]
    fn walkdir_sync_copies_tree() {
        let src = tempdir().unwrap();
        let dest = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("a")).unwrap();
        std::fs::write(src.path().join("a/file.txt"), "hi").unwrap();
        std::fs::write(src.path().join("ignored.log"), "x").unwrap();
        let filter = ExclusionFilter::new(vec!["*.log".into()]).unwrap();
        let stats = sync_walkdir(src.path(), dest.path(), &filter, false).unwrap();
        assert!(stats.files_copied >= 1);
        assert!(dest.path().join("a/file.txt").exists());
        assert!(!dest.path().join("ignored.log").exists());
    }

    #[test]
    fn walkdir_sync_prunes_removed_files() {
        let src = tempdir().unwrap();
        let dest = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "1").unwrap();
        std::fs::write(dest.path().join("stale.txt"), "2").unwrap();
        let filter = ExclusionFilter::new(vec![]).unwrap();
        sync_walkdir(src.path(), dest.path(), &filter, false).unwrap();
        assert!(!dest.path().join("stale.txt").exists());
        assert!(dest.path().join("a.txt").exists());
    }

    #[test]
    fn dry_run_makes_no_changes() {
        let src = tempdir().unwrap();
        let dest = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "1").unwrap();
        let filter = ExclusionFilter::new(vec![]).unwrap();
        sync_walkdir(src.path(), dest.path(), &filter, true).unwrap();
        assert!(!dest.path().join("a.txt").exists());
    }

    #[test]
    fn multi_location_uses_subdirs() {
        let src1 = tempdir().unwrap();
        let src2 = tempdir().unwrap();
        let dest = tempdir().unwrap();
        std::fs::write(src1.path().join("home.txt"), "1").unwrap();
        std::fs::write(src2.path().join("data.txt"), "2").unwrap();
        let agent = AgentConfig {
            key: "k".into(),
            display_name: "K".into(),
            category: crate::agent::AgentCategory::CliCoding,
            locations: vec![
                make_loc(src1.path().to_path_buf(), "home"),
                make_loc(src2.path().to_path_buf(), "data"),
            ],
        };
        let filter = ExclusionFilter::new(vec![]).unwrap();
        sync_agent_to_backup(&agent, dest.path(), &filter, false, false).unwrap();
        assert!(dest.path().join("home/home.txt").exists());
        assert!(dest.path().join("data/data.txt").exists());
    }

    /// Regression: a [`LocationKind::File`] backed up under a `backup_subdir`
    /// must land at `<subdir>/<basename>` as a FILE, not a directory.
    /// See bug-4 in scripts/check_features.sh.
    #[test]
    fn file_kind_location_backs_up_as_file() {
        let src = tempdir().unwrap();
        let dest = tempdir().unwrap();
        let file_src = src.path().join(".claude.json");
        std::fs::write(&file_src, "{}").unwrap();

        let agent = AgentConfig {
            key: "claude".into(),
            display_name: "Claude".into(),
            category: crate::agent::AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: file_src,
                location_type: LocationType::HomeDir,
                kind: LocationKind::File,
                backup_subdir: "root".into(),
            }],
        };
        let filter = ExclusionFilter::new(vec![]).unwrap();
        sync_agent_to_backup(&agent, dest.path(), &filter, false, false).unwrap();

        let copied = dest.path().join("root/.claude.json");
        assert!(copied.is_file(), "{copied:?} must be a file, not a dir");
        assert_eq!(std::fs::read_to_string(&copied).unwrap(), "{}");
    }

    /// Regression: a `.git` file or directory in the source must never be
    /// copied into the backup repo; only the destination's own `.git`
    /// (the backup repo metadata) is legitimate. See bug-1 / bug-2.
    #[test]
    fn walkdir_skips_dot_git_in_source() {
        let src = tempdir().unwrap();
        let dest = tempdir().unwrap();
        // Simulate a stray gitlink file left over by a previous restore.
        std::fs::write(src.path().join(".git"), "gitdir: /nonexistent\n").unwrap();
        std::fs::write(src.path().join("real.txt"), "keep").unwrap();
        // Pre-existing destination .git directory (mimics the backup repo).
        std::fs::create_dir_all(dest.path().join(".git/objects")).unwrap();
        std::fs::write(dest.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();

        let filter = ExclusionFilter::new(vec![]).unwrap();
        sync_walkdir(src.path(), dest.path(), &filter, false).unwrap();

        // The destination's own .git must be intact.
        assert!(dest.path().join(".git/HEAD").exists());
        // The source's .git must NOT have clobbered it.
        let head_contents = std::fs::read_to_string(dest.path().join(".git/HEAD")).unwrap();
        assert_eq!(head_contents, "ref: refs/heads/main");
        // Real files still get through.
        assert!(dest.path().join("real.txt").exists());
    }
}
