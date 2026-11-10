//! Per-agent and whole-backup orchestration: single shared git repo at backup root.
//!
//! Key invariants of the single-.git layout:
//! - All agents share ONE `.git/` directory at `backup_root/.git`.
//! - Agent content lives in agent-keyed subdirectories (`.claude/`, `.codex/`, …).
//! - `prune_stale_subdirs` is called ONCE after all agents sync, never per-agent.

use crate::agent::{AgentConfig, Registry};
use crate::config::Config;
use crate::error::{CasbError, Result};
use crate::filter::ExclusionFilter;
use crate::git::Repo;
use crate::hooks::{run_hooks, HookKind};
use crate::sync::{sync_agent_to_backup, SyncStats};
use crate::util::{expand_tilde, format_bytes};
use chrono::Utc;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Outcome of backing up a single agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupOutcome {
    /// Agent key.
    pub agent: String,
    /// Whether the agent had an installable source.
    pub installed: bool,
    /// Whether a new commit was actually created.
    pub committed: bool,
    /// Repo HEAD short hash after the operation (if any).
    pub commit: Option<String>,
    /// Stats from the file sync stage.
    pub sync: SyncStats,
    /// Non-fatal warnings for this agent (e.g. credential-shaped files that
    /// were excluded from the backup).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Error message, if the backup failed for this agent.
    pub error: Option<String>,
}

/// In the single-.git layout all agents share one repo at `backup_root`.
pub fn agent_repo_path(backup_root: &Path, _agent: &AgentConfig) -> PathBuf {
    backup_root.to_path_buf()
}

/// Backup all installed agents (or `keys` if non-empty).
///
/// The work is split into three phases:
///
/// 1. **Sync** — copy each agent's files into the backup repo. This is the
///    slow part and the only part that can safely run concurrently.
/// 2. **Commit** — stage and commit each agent's files. The backup repo is
///    shared by every agent, so it has exactly one git index: running this
///    phase concurrently makes git fail with `index.lock` contention.
/// 3. **Clean up** — remove gitignored leftovers and repack the repository,
///    once, after every agent has committed.
pub fn backup_agents(
    cfg: &Config,
    registry: &Registry,
    keys: &[String],
    message: Option<&str>,
    parallel: bool,
    dry_run: bool,
) -> Result<Vec<BackupOutcome>> {
    let backup_root = cfg.backup_root();
    if !backup_root.exists() && !dry_run {
        std::fs::create_dir_all(&backup_root)?;
        write_root_readme(&backup_root)?;
    }

    let agents: Vec<&AgentConfig> = if keys.is_empty() {
        registry.installed().collect()
    } else {
        keys.iter()
            .map(|k| registry.get(k))
            .collect::<Result<Vec<_>>>()?
    };

    if agents.is_empty() {
        return Ok(Vec::new());
    }

    // Initialize the single .git repo at backup_root (once, not per-agent).
    if !dry_run {
        let repo = Repo::new(&backup_root);
        if !repo.exists() {
            repo.init()?;
        } else {
            // Repos created before the safety pins existed (or restored
            // from an archive, whose sanitizer strips the pins) still get
            // global-config protection on every backup.
            repo.pin_safe_git_config()?;
        }
        // Refresh the repo's .gitignore every run so that newly added
        // exclusion patterns take effect on an existing backup repo.
        write_default_gitignore(
            &backup_root,
            &cfg.backup.exclusions,
            cfg.backup.include_secrets,
        )?;
    }

    // ── Phase 1: copy files (parallelisable) ─────────────────────────
    let sync_phase = |agent: &AgentConfig| -> SyncPhase {
        if !agent.is_installed() {
            return SyncPhase {
                installed: false,
                stats: SyncStats::default(),
                secrets: Vec::new(),
                error: None,
            };
        }
        let filter = match build_agent_filter(cfg, agent, &backup_root) {
            Ok(f) => f,
            Err(e) => {
                return SyncPhase {
                    installed: true,
                    stats: SyncStats::default(),
                    secrets: Vec::new(),
                    error: Some(e.to_string()),
                }
            }
        };
        if !dry_run {
            if let Err(e) = run_hooks(HookKind::PreBackup, &agent.key) {
                return SyncPhase {
                    installed: true,
                    stats: SyncStats::default(),
                    secrets: Vec::new(),
                    error: Some(e.to_string()),
                };
            }
        }
        match sync_agent_to_backup(agent, &backup_root, &filter, cfg.backup.use_rsync, dry_run) {
            Ok(stats) => {
                let secrets = scan_secret_files(agent, &filter);
                SyncPhase {
                    installed: true,
                    stats,
                    secrets,
                    error: None,
                }
            }
            Err(e) => SyncPhase {
                installed: true,
                stats: SyncStats::default(),
                secrets: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    };

    let phases: Vec<SyncPhase> = if parallel && !dry_run {
        agents.par_iter().map(|a| sync_phase(a)).collect()
    } else {
        agents.iter().map(|a| sync_phase(a)).collect()
    };

    // ── Phase 2: stage and commit (serial — one index for the repo) ──
    let repo = Repo::new(&backup_root);
    let mut outcomes: Vec<BackupOutcome> = Vec::with_capacity(agents.len());

    for (agent, phase) in agents.iter().zip(phases.iter()) {
        let mut committed = false;
        let mut commit = None;
        let mut error = phase.error.clone();

        if error.is_none() && !dry_run {
            // Scope the staging to this agent's own directories so a shared
            // repo cannot sweep one agent's files into another's commit.
            let subdirs = agent_backup_subdirs(agent);
            // Drop index entries for files the current filter excludes —
            // credential-shaped files committed before the secret
            // exclusions existed, or anything excluded since it was first
            // backed up. `git rm --cached` keeps the worktree copy; the
            // staged deletion rides along in this agent's commit.
            let untrack_result = build_agent_filter(cfg, agent, &backup_root)
                .and_then(|f| newly_excluded_tracked(&repo, agent, &f))
                .and_then(|stale| repo.untrack_paths(&stale));
            let stage_result = untrack_result.and_then(|_| {
                repo.add_paths(&subdirs).and_then(|_| {
                    let msg = message.map(|s| s.to_string()).unwrap_or_else(|| {
                        format!(
                            "casb backup {} at {}",
                            agent.key,
                            Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                        )
                    });
                    if cfg.general.auto_commit {
                        repo.commit(&msg)
                    } else {
                        Ok(false)
                    }
                })
            });
            match stage_result {
                Ok(was_committed) => {
                    committed = was_committed;
                    // Report the agent's *own* most recent commit, never the
                    // shared HEAD (which may belong to a different agent).
                    commit = repo.last_commit_for_paths(&subdirs)?;
                }
                Err(e) => error = Some(e.to_string()),
            }
        }

        // Count files actually tracked by git (after exclusions), not the
        // temporary rsync count which includes files that gitignore drops.
        let stats = if !dry_run && error.is_none() {
            crate::sync::count_git_tracked(&backup_root, &agent.backup_root_subdir())?
        } else {
            phase.stats.clone()
        };

        if !dry_run && error.is_none() {
            // Post-backup hooks are advisory: a failing hook must not
            // abort the remaining agents or skip the Phase-3 cleanup
            // (clean_ignored, location re-create, prune, compact). Record
            // it on the outcome like any other per-agent failure so the
            // command still reports non-zero for this agent.
            if let Err(e) = run_hooks(HookKind::PostBackup, &agent.key) {
                error = Some(format!("post-backup hook failed: {e}"));
            }
        }

        if !phase.installed && error.is_none() {
            error = Some("not installed".to_string());
        }

        // Surface credential-shaped files that were kept out of the backup.
        // The repo is plaintext git — users must know secrets were skipped
        // (and how to opt back in) rather than discover it in git log.
        let mut warnings = Vec::new();
        if !phase.secrets.is_empty() {
            let n = phase.secrets.len();
            let sample: Vec<&str> = phase.secrets.iter().take(5).map(String::as_str).collect();
            let more = if n > sample.len() {
                format!(", +{} more", n - sample.len())
            } else {
                String::new()
            };
            warnings.push(format!(
                "{n} credential-shaped file(s) excluded from backup ({}{}); \
                 set backup.include_secrets=true to include them",
                sample.join(", "),
                more
            ));
        }

        outcomes.push(BackupOutcome {
            agent: agent.key.clone(),
            installed: phase.installed,
            committed,
            commit,
            sync: stats,
            warnings,
            error,
        });
    }

    // ── Phase 3: clean up once, after every agent has committed ──────
    if !dry_run {
        // Remove files excluded by .gitignore from disk (packages/,
        // plugins/, computer-use/) that rsync may have copied but git
        // refused to track — keeps backup root lean (~MB not GB).
        if std::env::var("CASB_DEBUG_CLEAN").is_ok() {
            let out = std::process::Command::new("git")
                .current_dir(&backup_root)
                .args(["clean", "-fdX", "-n"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            eprintln!("[clean DEBUG] would remove:\n{}", out);
        }
        repo.clean_ignored()?;

        // `git clean -fdX` removes a directory whose entire contents are
        // gitignored, and rsync's pattern handling does not always agree
        // with git's — so an excluded directory can survive the copy and be
        // cleaned afterwards, taking its location directory with it.
        // Re-create the location directories so the backup tree always
        // shows where each agent's data lives.
        for agent in agents.iter() {
            for loc in agent.installed_locations() {
                if !matches!(loc.kind, crate::agent::LocationKind::Directory) {
                    continue;
                }
                let dest = crate::sync::backup_dest_for(&backup_root, loc);
                if !dest.exists() {
                    let _ = std::fs::create_dir_all(&dest);
                }
            }
        }

        // Prune stale agent subdirs after all agents have synced. The set of
        // live directories is derived from *every installed agent*, not just
        // the ones in this run: backing up a single agent must not delete
        // another installed agent's directory from the working tree. Compare
        // top-level names: a multi-location agent such as Claude stores its
        // data under `.claude/home`, `.claude/data` and `.claude/root`, and
        // matching the full subdirectory path would make `.claude` look
        // stale and delete the agent's whole backup.
        let known_subdirs: HashSet<String> = registry
            .installed()
            .flat_map(|a| a.installed_locations())
            .map(|l| top_level_subdir(&l.backup_subdir))
            .collect();
        crate::sync::prune_stale_subdirs(&known_subdirs, &backup_root)?;

        repo.compact()?;
    }

    Ok(outcomes)
}

/// Result of the file-copy phase for one agent.
#[derive(Debug, Clone)]
struct SyncPhase {
    installed: bool,
    stats: SyncStats,
    /// Credential-shaped files found in the source that the filter excluded.
    secrets: Vec<String>,
    error: Option<String>,
}

/// The first path component of a backup subdirectory (`.claude` for
/// `.claude/home`).
fn top_level_subdir(subdir: &str) -> String {
    subdir
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(subdir)
        .to_string()
}

/// The top-level backup directories an agent occupies.
pub(crate) fn agent_backup_subdirs(agent: &AgentConfig) -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();
    for loc in &agent.locations {
        let top = top_level_subdir(&loc.backup_subdir);
        if !dirs.contains(&top) {
            dirs.push(top);
        }
    }
    if dirs.is_empty() {
        dirs.push(agent.backup_root_subdir());
    }
    dirs
}

/// Build the exclusion filter for one agent.
///
/// The same filter must be used for backup, diff and restore. If restore
/// used a different (or empty) filter it would delete every excluded path
/// from the live source directory.
pub fn build_agent_filter(
    cfg: &Config,
    agent: &AgentConfig,
    backup_root: &Path,
) -> Result<ExclusionFilter> {
    let agent_extras = cfg
        .agents
        .get(&agent.key)
        .map(|a| a.exclusions.clone())
        .unwrap_or_default();
    let mut filter = ExclusionFilter::from_layers(
        &crate::config::default_exclusions(),
        &cfg.backup.exclusions,
        &agent_extras,
    )?
    .with_max_file_size(max_file_size_bytes(cfg));
    // Credential-shaped files are excluded by default: the backup repo is
    // plaintext git, so committing `auth.json`/`*.pem`/`.env*` would leak
    // live secrets into history permanently. `backup.include_secrets=true`
    // opts back in.
    if !cfg.backup.include_secrets {
        filter.add_secret_patterns(&crate::config::default_secret_patterns())?;
    }
    for loc in agent.installed_locations() {
        if matches!(loc.kind, crate::agent::LocationKind::Directory) {
            let candidate = loc.path.join(".casbignore");
            filter.merge_casbignore_file(&candidate)?;
        }
    }
    filter.merge_casbignore_file(&backup_root.join(".casbignore"))?;
    Ok(filter)
}

/// Repo-relative tracked paths under the agent's subdirs that the current
/// filter excludes — i.e. files committed before an exclusion (ordinary or
/// credential-shaped) existed. Callers untrack them so plaintext secrets
/// and stale excluded files leave the index.
fn newly_excluded_tracked(
    repo: &Repo,
    agent: &AgentConfig,
    filter: &ExclusionFilter,
) -> Result<Vec<String>> {
    let subdirs = agent_backup_subdirs(agent);
    let tracked = repo.ls_files(&subdirs)?;
    let mut stale = Vec::new();
    for path in tracked {
        if let Some(rel) = location_relative_path(agent, &path) {
            if filter.is_excluded(&rel) {
                stale.push(path);
            }
        }
    }
    Ok(stale)
}

/// Map a repo-relative tracked path back to the path *inside* its agent
/// location, using the longest matching `backup_subdir` prefix. Returns
/// `None` for paths outside every known location subdir (e.g. leftovers of
/// an old backup layout).
fn location_relative_path(agent: &AgentConfig, repo_path: &str) -> Option<PathBuf> {
    let mut best: Option<(&crate::agent::AgentLocation, usize)> = None;
    for loc in &agent.locations {
        let sub = loc.backup_subdir.trim_end_matches('/');
        if sub.is_empty() {
            continue;
        }
        let prefix = format!("{sub}/");
        if repo_path == sub || repo_path.starts_with(&prefix) {
            if best.map(|(_, s)| sub.len() > s).unwrap_or(true) {
                best = Some((loc, sub.len()));
            }
        }
    }
    let (loc, _) = best?;
    let sub = loc.backup_subdir.trim_end_matches('/');
    let rest = repo_path.strip_prefix(sub)?.trim_start_matches('/');
    if rest.is_empty() {
        return None;
    }
    Some(PathBuf::from(rest))
}

/// Credential-shaped files in the agent's installed locations that the
/// filter excludes. Capped for reporting; used for backup warnings and
/// the doctor check. Paths are reported as `<backup_subdir>/<relpath>`.
pub(crate) fn scan_secret_files(agent: &AgentConfig, filter: &ExclusionFilter) -> Vec<String> {
    const MAX_REPORT: usize = 50;
    let mut hits: Vec<String> = Vec::new();
    'outer: for loc in agent.installed_locations() {
        match loc.kind {
            crate::agent::LocationKind::File => {
                let name = loc
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !name.is_empty() && filter.is_secret_excluded(Path::new(&name)) {
                    hits.push(format!("{}/{}", loc.backup_subdir, name));
                    if hits.len() >= MAX_REPORT {
                        break 'outer;
                    }
                }
            }
            crate::agent::LocationKind::Directory => {
                let walker = walkdir::WalkDir::new(&loc.path)
                    .follow_links(false)
                    .into_iter();
                for entry in walker
                    .filter_entry(|e| {
                        if e.depth() == 0 {
                            return true;
                        }
                        // Never descend into .git or into directories the
                        // filter excludes — same pruning the sync applies.
                        if e.file_name() == ".git" {
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
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let rel = match entry.path().strip_prefix(&loc.path) {
                        Ok(r) => r.to_path_buf(),
                        Err(_) => continue,
                    };
                    if filter.is_secret_excluded(&rel) {
                        hits.push(format!("{}/{}", loc.backup_subdir, rel.display()));
                        if hits.len() >= MAX_REPORT {
                            break 'outer;
                        }
                    }
                }
            }
        }
    }
    hits
}

/// The configured per-file size cap in bytes. `0` disables the cap.
pub fn max_file_size_bytes(cfg: &Config) -> Option<u64> {
    match cfg.backup.max_file_size_mb {
        0 => None,
        mb => Some(mb.saturating_mul(1024 * 1024)),
    }
}

/// Initialise the backup root directory.
pub fn init_backup_root(cfg: &Config, override_root: Option<&Path>) -> Result<PathBuf> {
    let root = match override_root {
        Some(p) => expand_tilde(p),
        None => cfg.backup_root(),
    };
    std::fs::create_dir_all(&root)?;
    write_root_readme(&root)?;
    Ok(root)
}

fn write_root_readme(root: &Path) -> Result<()> {
    let path = root.join("README.md");
    if path.exists() {
        return Ok(());
    }
    let body = format!(
        "# Coding Agent Settings Backups\n\n\
         This directory is managed by `casb` (coding_agent_settings_backup).\n\
         All agents share a single git repository at this root; agent-specific\n\
         content lives in agent-keyed subdirectories (e.g. `.claude/`, `.codex/`).\n\n\
         Created at: {}\n",
        Utc::now().to_rfc3339()
    );
    std::fs::write(path, body)?;
    Ok(())
}

fn write_default_gitignore(repo: &Path, extras: &[String], include_secrets: bool) -> Result<()> {
    let path = repo.join(".gitignore");
    let mut existing = String::new();
    if path.exists() {
        existing = std::fs::read_to_string(&path)?;
    }
    let mut lines: Vec<String> = existing.lines().map(|s| s.to_string()).collect();
    let defaults = crate::config::default_exclusions();
    // Secret patterns go into .gitignore too (when secrets are excluded) so
    // a file that was committed before the exclusion existed — or one that
    // appears by some other route — is never re-tracked. `**/name` gitignore
    // syntax matches at any depth, same as the filter.
    let secrets = if include_secrets {
        Vec::new()
    } else {
        crate::config::default_secret_patterns()
    };
    for pat in defaults.iter().chain(extras.iter()).chain(secrets.iter()) {
        if !lines.iter().any(|l| l.trim() == pat.trim()) {
            lines.push(pat.clone());
        }
    }
    let body = lines.join("\n") + "\n";
    std::fs::write(path, body)?;
    Ok(())
}

/// Ensure the backup repo exists and returns a [`Repo`] handle.
pub fn require_repo(cfg: &Config) -> Result<Repo> {
    let repo = Repo::new(cfg.backup_root());
    if !repo.exists() {
        return Err(CasbError::NoBackupRepo {
            key: "backup root".into(),
        });
    }
    // Repos created before the safety pins existed (or restored from an
    // older archive) still get global-config protection on every command.
    repo.pin_safe_git_config()?;
    Ok(repo)
}

/// Format a single [`BackupOutcome`] as a human-readable line.
pub fn format_outcome_text(outcome: &BackupOutcome) -> String {
    if let Some(err) = &outcome.error {
        return format!("✗ {} — {err}", outcome.agent);
    }
    if !outcome.installed {
        return format!("• {} — not installed", outcome.agent);
    }
    let bytes = format_bytes(outcome.sync.bytes_copied);
    match outcome.commit.as_deref() {
        Some(commit) => {
            let label = if outcome.committed {
                "committed"
            } else {
                "no changes"
            };
            format!(
                "✓ {} — {} files, {bytes} ({commit}, {label})",
                outcome.agent, outcome.sync.files_copied,
            )
        }
        // The agent has nothing in the repository yet — no commit to point
        // at, and no other agent's commit to borrow.
        None => format!(
            "✓ {} — {} files, {bytes} (nothing backed up yet)",
            outcome.agent, outcome.sync.files_copied,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentCategory, AgentLocation, LocationKind, LocationType};
    use tempfile::tempdir;

    fn make_agent(key: &str, src: &std::path::Path) -> AgentConfig {
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

    fn cfg_for(root: &std::path::Path) -> Config {
        let mut c = Config::default();
        c.general.backup_root = root.display().to_string();
        c.backup.use_rsync = false;
        c
    }

    #[test]
    fn agent_repo_path_returns_backup_root() {
        let agent = AgentConfig {
            key: "k".into(),
            display_name: "K".into(),
            category: AgentCategory::CliCoding,
            locations: vec![],
        };
        let p = agent_repo_path(std::path::Path::new("/tmp"), &agent);
        assert_eq!(p, std::path::PathBuf::from("/tmp"));
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
    fn backup_creates_repo_and_commits() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "hello").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("test1", src.path());

        // Unknown key bubbles up.
        let outcomes = backup_agents(
            &cfg,
            &Registry::from_config(&Config::default()).unwrap(),
            &["nonexistent".into()],
            None,
            false,
            false,
        );
        assert!(outcomes.is_err());

        let reg = registry_with(&agent);
        let outcomes =
            backup_agents(&cfg, &reg, &["test1".into()], Some("msg"), false, false).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].installed);
        assert!(outcomes[0].committed);
        assert!(outcomes[0].error.is_none());
        // .git is at backup_root; file lands in .test1/ subdir.
        assert!(backup_root.path().join(".git").exists());
        assert!(backup_root.path().join(".test1").join("a.txt").exists());
    }

    #[test]
    fn backup_dry_run_does_not_commit() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "x").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("dry", src.path());
        let reg = registry_with(&agent);

        let outcomes = backup_agents(&cfg, &reg, &["dry".into()], None, false, true).unwrap();
        assert!(outcomes[0].installed);
        assert!(!outcomes[0].committed);
        // Dry run must not create the repo or copy any file.
        assert!(!backup_root.path().join(".git").exists());
        assert!(!backup_root.path().join(".dry").join("a.txt").exists());
    }

    /// Regression: a multi-location agent stores its data under a nested
    /// path (`.claude/home`, `.claude/data`). The stale-subdirectory prune
    /// compares top-level names, so it must not delete `.claude`.
    #[test]
    fn multi_location_agent_dir_survives_prune() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir_all(src.path().join("data")).unwrap();
        std::fs::write(src.path().join("data/d.txt"), "data").unwrap();
        let cfg = cfg_for(backup_root.path());

        let agent = AgentConfig {
            key: "claude".into(),
            display_name: "Claude".into(),
            category: AgentCategory::CliCoding,
            locations: vec![
                AgentLocation {
                    path: src.path().to_path_buf(),
                    location_type: LocationType::HomeDir,
                    kind: LocationKind::Directory,
                    backup_subdir: ".claude/home".into(),
                },
                AgentLocation {
                    path: src.path().join("data").to_path_buf(),
                    location_type: LocationType::HomeDir,
                    kind: LocationKind::Directory,
                    backup_subdir: ".claude/data".into(),
                },
            ],
        };
        let reg = Registry::from_agents(vec![agent]);
        backup_agents(&cfg, &reg, &["claude".into()], None, false, false).unwrap();

        assert!(
            backup_root
                .path()
                .join(".claude")
                .join("home")
                .join("a.txt")
                .exists(),
            "multi-location agent data must survive the prune"
        );
        assert!(
            backup_root.path().join(".claude").join("data").exists(),
            "every location directory must survive the backup"
        );
    }

    /// Regression: a location whose entire content is excluded must still
    /// leave its directory in the backup tree. `git clean -fdX` removes
    /// directories whose contents are all gitignored.
    #[test]
    fn location_directory_survives_fully_excluded_content() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("versions")).unwrap();
        std::fs::write(src.path().join("versions/current.json"), "{}").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("excl", src.path());
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["excl".into()], None, false, false).unwrap();

        assert!(
            backup_root.path().join(".excl").exists(),
            "the agent's location directory must remain in the backup"
        );
        assert!(
            !backup_root.path().join(".excl/versions").exists(),
            "excluded content must not be in the backup"
        );
    }

    /// Regression: `casb backup --parallel` must not fail with git
    /// `index.lock` contention. The file copy runs concurrently, the git
    /// index is used one agent at a time.
    #[test]
    fn parallel_backup_commits_every_agent() {
        let backup_root = tempdir().unwrap();
        let mut cfg = cfg_for(backup_root.path());
        cfg.backup.use_rsync = true;
        // The source directories must outlive the backup run.
        let sources: Vec<_> = ["p1", "p2", "p3", "p4", "p5", "p6"]
            .iter()
            .map(|name| {
                let src = tempdir().unwrap();
                std::fs::write(src.path().join("f.txt"), name).unwrap();
                src
            })
            .collect();
        let agents: Vec<AgentConfig> = sources
            .iter()
            .enumerate()
            .map(|(i, src)| {
                let key = format!("p{}", i + 1);
                AgentConfig {
                    key: key.clone(),
                    display_name: key.clone(),
                    category: AgentCategory::CliCoding,
                    locations: vec![AgentLocation {
                        path: src.path().to_path_buf(),
                        location_type: LocationType::HomeDir,
                        kind: LocationKind::Directory,
                        backup_subdir: format!(".{key}"),
                    }],
                }
            })
            .collect();
        let keys: Vec<String> = agents.iter().map(|a| a.key.clone()).collect();
        let reg = Registry::from_agents(agents);
        let outcomes = backup_agents(&cfg, &reg, &keys, None, true, false).unwrap();
        assert_eq!(outcomes.len(), keys.len());
        for o in &outcomes {
            assert!(o.error.is_none(), "{} failed: {:?}", o.agent, o.error);
            assert!(o.committed, "{} was not committed", o.agent);
        }
        let repo = Repo::new(backup_root.path());
        assert_eq!(repo.commit_count().unwrap(), keys.len() as u64);
    }

    /// Regression: oversized files must never reach the backup repo. Agent
    /// directories contain reinstallable binaries (macOS `.app` bundles,
    /// downloaded CLI versions) that would otherwise be committed and bloat
    /// the repository by hundreds of megabytes.
    #[test]
    fn oversized_files_are_not_backed_up() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("small.txt"), "ok").unwrap();
        let big = src.path().join("big.bin");
        std::fs::write(&big, vec![0u8; 2 * 1024 * 1024]).unwrap();

        let mut cfg = cfg_for(backup_root.path());
        cfg.backup.max_file_size_mb = 1; // 1 MiB cap
        let agent = make_agent("sized", src.path());
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["sized".into()], None, false, false).unwrap();

        assert!(backup_root.path().join(".sized").join("small.txt").exists());
        assert!(
            !backup_root.path().join(".sized").join("big.bin").exists(),
            "oversized file must not be copied into the backup"
        );
        let tracked = std::process::Command::new("git")
            .current_dir(backup_root.path())
            .args(["ls-files", ".sized"])
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&tracked.stdout);
        assert!(
            !listed.contains("big.bin"),
            "oversized file must not be tracked"
        );
    }

    #[test]
    fn require_repo_errors_when_missing() {
        let backup_root = tempdir().unwrap();
        let cfg = cfg_for(backup_root.path());
        let err = require_repo(&cfg).unwrap_err();
        matches!(err, CasbError::NoBackupRepo { .. });
    }

    #[test]
    fn format_outcome_text_variants() {
        let installed = BackupOutcome {
            agent: "a".into(),
            installed: true,
            committed: true,
            commit: Some("abcdef0".into()),
            sync: SyncStats {
                files_copied: 3,
                bytes_copied: 1024,
                ..Default::default()
            },
            warnings: vec![],
            error: None,
        };
        let s = format_outcome_text(&installed);
        assert!(s.contains("✓"));
        assert!(s.contains("3 files"));
        assert!(s.contains("abcdef0"));

        let not_installed = BackupOutcome {
            agent: "b".into(),
            installed: false,
            committed: false,
            commit: None,
            sync: Default::default(),
            warnings: vec![],
            error: None,
        };
        assert!(format_outcome_text(&not_installed).contains("not installed"));

        let errored = BackupOutcome {
            agent: "c".into(),
            installed: true,
            committed: false,
            commit: None,
            sync: Default::default(),
            warnings: vec![],
            error: Some("boom".into()),
        };
        assert!(format_outcome_text(&errored).contains("boom"));
    }

    // ----- credential-shaped file exclusions (secrets bead) -----

    fn tracked_files(repo: &Path, subdir: &str) -> String {
        let out = std::process::Command::new("git")
            .current_dir(repo)
            .args(["ls-files", "--", subdir])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Regression (bead secrets-committed-plaintext-fqa): credential-shaped
    /// files must never reach the plaintext git backup — not in the
    /// worktree, not in the index — and the backup must warn about them.
    #[test]
    fn secrets_are_excluded_warned_and_untracked() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        for name in [
            ".credentials.json",
            "auth.json",
            "server.pem",
            "id_rsa",
            ".env",
            ".env.local",
            "normal.txt",
        ] {
            std::fs::write(src.path().join(name), "SECRET").unwrap();
        }
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("sec", src.path());
        let reg = registry_with(&agent);

        // First back up WITH secrets included so they land in history —
        // simulating a repo created before the exclusion existed.
        let mut cfg_with = cfg_for(backup_root.path());
        cfg_with.backup.include_secrets = true;
        backup_agents(&cfg_with, &reg, &["sec".into()], None, false, false).unwrap();
        let tracked = tracked_files(backup_root.path(), ".sec");
        assert!(tracked.contains("auth.json"), "precondition: {tracked}");

        // Now back up with the default (secrets excluded).
        let outcomes = backup_agents(
            &cfg,
            &reg,
            &["sec".into()],
            Some("no secrets"),
            false,
            false,
        )
        .unwrap();
        let o = &outcomes[0];
        assert!(o.error.is_none());
        assert!(
            !o.warnings.is_empty(),
            "backup must warn about excluded secrets"
        );
        assert!(o.warnings[0].contains("credential-shaped"));

        let tracked = tracked_files(backup_root.path(), ".sec");
        for secret in [
            ".credentials.json",
            "auth.json",
            "server.pem",
            "id_rsa",
            ".env",
            ".env.local",
        ] {
            assert!(
                !tracked.contains(secret),
                "{secret} must be untracked: {tracked}"
            );
        }
        assert!(tracked.contains("normal.txt"), "{tracked}");
        // The worktree copies are gone too (git clean -fdX removes the
        // now-ignored files), but the live source is untouched.
        assert!(src.path().join("auth.json").exists());
    }

    /// `backup.include_secrets = true` opts back in: secrets are copied,
    /// tracked, and no warning is emitted.
    #[test]
    fn include_secrets_backs_up_credentials() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("auth.json"), "{}").unwrap();
        std::fs::write(src.path().join("notes.txt"), "n").unwrap();
        let mut cfg = cfg_for(backup_root.path());
        cfg.backup.include_secrets = true;
        let agent = make_agent("sec2", src.path());
        let reg = registry_with(&agent);
        let outcomes = backup_agents(&cfg, &reg, &["sec2".into()], None, false, false).unwrap();
        assert!(outcomes[0].warnings.is_empty());
        assert!(outcomes[0].error.is_none());
        let tracked = tracked_files(backup_root.path(), ".sec2");
        assert!(tracked.contains("auth.json"), "{tracked}");
    }
}
