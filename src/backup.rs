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
        }
        // Refresh the repo's .gitignore every run so that newly added
        // exclusion patterns take effect on an existing backup repo.
        write_default_gitignore(&backup_root, &cfg.backup.exclusions)?;
    }

    // ── Phase 1: copy files (parallelisable) ─────────────────────────
    let sync_phase = |agent: &AgentConfig| -> SyncPhase {
        if !agent.is_installed() {
            return SyncPhase {
                installed: false,
                stats: SyncStats::default(),
                error: None,
            };
        }
        let filter = match build_agent_filter(cfg, agent, &backup_root) {
            Ok(f) => f,
            Err(e) => {
                return SyncPhase {
                    installed: true,
                    stats: SyncStats::default(),
                    error: Some(e.to_string()),
                }
            }
        };
        if !dry_run {
            if let Err(e) = run_hooks(HookKind::PreBackup, &agent.key) {
                return SyncPhase {
                    installed: true,
                    stats: SyncStats::default(),
                    error: Some(e.to_string()),
                };
            }
        }
        match sync_agent_to_backup(agent, &backup_root, &filter, cfg.backup.use_rsync, dry_run) {
            Ok(stats) => SyncPhase {
                installed: true,
                stats,
                error: None,
            },
            Err(e) => SyncPhase {
                installed: true,
                stats: SyncStats::default(),
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
            let stage_result = repo.add_paths(&subdirs).and_then(|_| {
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
            run_hooks(HookKind::PostBackup, &agent.key)?;
        }

        if !phase.installed && error.is_none() {
            error = Some("not installed".to_string());
        }

        outcomes.push(BackupOutcome {
            agent: agent.key.clone(),
            installed: phase.installed,
            committed,
            commit,
            sync: stats,
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
fn agent_backup_subdirs(agent: &AgentConfig) -> Vec<String> {
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
    for loc in agent.installed_locations() {
        if matches!(loc.kind, crate::agent::LocationKind::Directory) {
            let candidate = loc.path.join(".casbignore");
            filter.merge_casbignore_file(&candidate)?;
        }
    }
    filter.merge_casbignore_file(&backup_root.join(".casbignore"))?;
    Ok(filter)
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

fn write_default_gitignore(repo: &Path, extras: &[String]) -> Result<()> {
    let path = repo.join(".gitignore");
    let mut existing = String::new();
    if path.exists() {
        existing = std::fs::read_to_string(&path)?;
    }
    let mut lines: Vec<String> = existing.lines().map(|s| s.to_string()).collect();
    let defaults = crate::config::default_exclusions();
    for pat in defaults.iter().chain(extras.iter()) {
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
    let commit = outcome.commit.as_deref().unwrap_or("(no commits)");
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
            error: None,
        };
        assert!(format_outcome_text(&not_installed).contains("not installed"));

        let errored = BackupOutcome {
            agent: "c".into(),
            installed: true,
            committed: false,
            commit: None,
            sync: Default::default(),
            error: Some("boom".into()),
        };
        assert!(format_outcome_text(&errored).contains("boom"));
    }
}
