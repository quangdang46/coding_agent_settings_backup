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
            write_default_gitignore(&backup_root, &cfg.backup.exclusions)?;
        }
    }

    let do_one = |agent: &AgentConfig| -> BackupOutcome {
        let res = backup_one(cfg, agent, &backup_root, message, dry_run);
        match res {
            Ok(mut outcome) => {
                if !outcome.installed {
                    outcome.error = Some("not installed".to_string());
                }
                outcome
            }
            Err(e) => BackupOutcome {
                agent: agent.key.clone(),
                installed: agent.is_installed(),
                committed: false,
                commit: None,
                sync: SyncStats::default(),
                error: Some(e.to_string()),
            },
        }
    };

    let outcomes: Vec<BackupOutcome> = if parallel && !dry_run {
        agents.par_iter().map(|a| do_one(a)).collect()
    } else {
        agents.iter().map(|a| do_one(a)).collect()
    };

    // Prune stale agent subdirs after all agents have synced.
    if !dry_run {
        let known_subdirs: HashSet<String> = agents
            .iter()
            .flat_map(|a| a.installed_locations())
            .map(|l| l.backup_subdir.clone())
            .collect();
        crate::sync::prune_stale_subdirs(&known_subdirs, &backup_root)?;
    }

    Ok(outcomes)
}

fn backup_one(
    cfg: &Config,
    agent: &AgentConfig,
    backup_root: &Path,
    message: Option<&str>,
    dry_run: bool,
) -> Result<BackupOutcome> {
    if !agent.is_installed() {
        return Ok(BackupOutcome {
            agent: agent.key.clone(),
            installed: false,
            committed: false,
            commit: None,
            sync: SyncStats::default(),
            error: None,
        });
    }
    let repo = Repo::new(backup_root);

    let agent_extras = cfg
        .agents
        .get(&agent.key)
        .map(|a| a.exclusions.clone())
        .unwrap_or_default();
    let mut filter = ExclusionFilter::from_layers(
        &crate::config::default_exclusions(),
        &cfg.backup.exclusions,
        &agent_extras,
    )?;
    for loc in agent.installed_locations() {
        if matches!(loc.kind, crate::agent::LocationKind::Directory) {
            let candidate = loc.path.join(".casbignore");
            filter.merge_casbignore_file(&candidate)?;
        }
    }
    filter.merge_casbignore_file(&backup_root.join(".casbignore"))?;

    if !dry_run {
        run_hooks(HookKind::PreBackup, &agent.key)?;
    }

    let _stats = sync_agent_to_backup(agent, backup_root, &filter, cfg.backup.use_rsync, dry_run)?;

    let mut committed = false;
    let mut commit = None;
    if !dry_run && cfg.general.auto_commit {
        repo.add_all()?;
        let msg = message.map(|s| s.to_string()).unwrap_or_else(|| {
            format!(
                "casb backup {} at {}",
                agent.key,
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
            )
        });
        committed = repo.commit(&msg)?;
        commit = repo.head_short()?;
    } else if !dry_run {
        repo.add_all()?;
        commit = repo.head_short()?;
    }

    // Count files actually tracked by git (after exclusions), not the
    // temporary rsync count which includes files that gitignore drops.
    let stats = if !dry_run && commit.is_some() {
        crate::sync::count_git_tracked(backup_root, &agent.locations[0].backup_subdir)?
    } else {
        _stats
    };

    if !dry_run {
        run_hooks(HookKind::PostBackup, &agent.key)?;
    }

    Ok(BackupOutcome {
        agent: agent.key.clone(),
        installed: true,
        committed,
        commit,
        sync: stats,
        error: None,
    })
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

    #[test]
    fn backup_one_creates_repo_and_commits() {
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

        // Initialize the single .git repo (done by backup_agents in real usage).
        let repo = Repo::new(backup_root.path());
        repo.init().unwrap();
        let outcome = backup_one(&cfg, &agent, backup_root.path(), Some("msg"), false).unwrap();
        assert!(outcome.installed);
        assert!(outcome.committed);
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
        let repo = Repo::new(backup_root.path());
        repo.init().unwrap();

        let outcome = backup_one(&cfg, &agent, backup_root.path(), None, true).unwrap();
        assert!(outcome.installed);
        assert!(!outcome.committed);
        // Dry run does not stage new changes.
        assert!(!outcome.committed);
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
