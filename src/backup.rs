//! `casb backup` implementation.
//!
//! Per-agent: create the backup repo if missing, sync the agent's source
//! locations into it (respecting exclusions), and commit the result.

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

/// Compute the backup repo path for a given agent.
pub fn agent_repo_path(backup_root: &Path, agent: &AgentConfig) -> PathBuf {
    backup_root.join(format!(".{}", agent.key))
}

/// Backup all installed agents (or `keys` if non-empty).
///
/// Returns one [`BackupOutcome`] per agent considered.
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
    let repo_path = agent_repo_path(backup_root, agent);
    let repo = Repo::new(&repo_path);
    if !dry_run {
        if !repo.exists() {
            repo.init()?;
            write_repo_readme(&repo_path, agent)?;
        }
        write_default_gitignore(&repo_path, &cfg.backup.exclusions)?;
    }

    // Build filter: defaults + extras + per-agent extras (from config).
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
    // Also merge `.casbignore` files at the source roots.
    for loc in agent.installed_locations() {
        if matches!(loc.kind, crate::agent::LocationKind::Directory) {
            let candidate = loc.path.join(".casbignore");
            filter.merge_casbignore_file(&candidate)?;
        }
    }
    // And from the backup repo itself.
    filter.merge_casbignore_file(&repo_path.join(".casbignore"))?;

    // Hooks (pre-backup), only when not dry-run.
    if !dry_run {
        run_hooks(HookKind::PreBackup, &agent.key)?;
    }

    let stats = sync_agent_to_backup(agent, &repo_path, &filter, cfg.backup.use_rsync, dry_run)?;

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
         Each subdirectory beginning with `.` is a per-agent git repository\n\
         containing snapshots of that agent's configuration over time.\n\n\
         Created at: {}\n",
        Utc::now().to_rfc3339()
    );
    std::fs::write(path, body)?;
    Ok(())
}

fn write_repo_readme(repo: &Path, agent: &AgentConfig) -> Result<()> {
    let path = repo.join("README.md");
    if path.exists() {
        return Ok(());
    }
    let mut body = format!(
        "# {}\n\nManaged by `casb`. Source locations:\n\n",
        agent.display_name
    );
    for loc in &agent.locations {
        body.push_str(&format!(
            "- {} ({:?})\n",
            loc.path.display(),
            loc.location_type
        ));
    }
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

/// Ensure a backup repo exists and returns a [`Repo`] handle.
pub fn require_repo(cfg: &Config, agent: &AgentConfig) -> Result<Repo> {
    let path = agent_repo_path(&cfg.backup_root(), agent);
    let repo = Repo::new(path);
    if !repo.exists() {
        return Err(CasbError::NoBackupRepo {
            key: agent.key.clone(),
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
                backup_subdir: ".".into(),
            }],
        }
    }

    fn cfg_for(root: &std::path::Path) -> Config {
        let mut c = Config::default();
        c.general.backup_root = root.display().to_string();
        // Force walkdir backend in tests so behaviour is deterministic.
        c.backup.use_rsync = false;
        c
    }

    #[test]
    fn agent_repo_path_uses_dot_prefix() {
        let agent = AgentConfig {
            key: "k".into(),
            display_name: "K".into(),
            category: AgentCategory::CliCoding,
            locations: vec![],
        };
        let p = agent_repo_path(std::path::Path::new("/tmp"), &agent);
        assert_eq!(p, std::path::PathBuf::from("/tmp/.k"));
    }

    #[test]
    fn backup_one_creates_repo_and_commits() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "hello").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("test1", src.path());

        let outcomes = backup_agents(
            &cfg,
            &Registry::from_config(&Config::default()).unwrap(),
            &["nonexistent".into()],
            None,
            false,
            false,
        );
        // unknown key bubbles up as AgentNotFound
        assert!(outcomes.is_err());

        // Direct call — sync via internal helper.
        let repo_path = agent_repo_path(backup_root.path(), &agent);
        let _ = std::fs::create_dir_all(&repo_path);
        let outcome = backup_one(&cfg, &agent, backup_root.path(), Some("msg"), false).unwrap();
        assert!(outcome.installed);
        assert!(outcome.committed);
        assert!(repo_path.join(".git").exists());
        assert!(repo_path.join("a.txt").exists());
    }

    #[test]
    fn backup_dry_run_makes_no_changes() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "x").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = make_agent("dry", src.path());
        let outcome = backup_one(&cfg, &agent, backup_root.path(), None, true).unwrap();
        assert!(outcome.installed);
        // Dry run does not commit.
        assert!(!outcome.committed);
        let repo = agent_repo_path(backup_root.path(), &agent);
        assert!(!repo.join(".git").exists());
    }

    #[test]
    fn require_repo_errors_when_missing() {
        let backup_root = tempdir().unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "missing".into(),
            display_name: "M".into(),
            category: AgentCategory::CliCoding,
            locations: vec![],
        };
        let err = require_repo(&cfg, &agent).unwrap_err();
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
