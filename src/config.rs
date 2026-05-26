//! Configuration loader for casb.
//!
//! Reads TOML config from `~/.config/casb/config.toml` (overridable via
//! `CASB_CONFIG`), with environment-variable overrides for the most common
//! settings. Unknown TOML fields are tolerated to allow forward compatibility.

use crate::error::{CasbError, Result};
use crate::util::{expand_tilde, home_dir};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Top-level configuration object.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Global settings.
    pub general: GeneralConfig,

    /// Backup-specific settings.
    pub backup: BackupConfig,

    /// Schedule defaults.
    pub schedule: ScheduleConfig,

    /// Custom or overridden agents, keyed by agent key.
    pub agents: BTreeMap<String, CustomAgentConfig>,
}

/// `[general]` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Backup root directory (may contain `~`).
    pub backup_root: String,
    /// Auto-commit on each `casb backup` invocation.
    pub auto_commit: bool,
    /// Default verbose flag.
    pub verbose: bool,
    /// Default quiet flag.
    pub quiet: bool,
    /// Default output format (`text`, `json`, `toon`).
    pub output_format: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            backup_root: "~/.agent_settings_backups".into(),
            auto_commit: true,
            verbose: false,
            quiet: false,
            output_format: "text".into(),
        }
    }
}

/// `[backup]` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupConfig {
    /// Default exclusion glob patterns applied to all agents.
    pub exclusions: Vec<String>,
    /// Use `rsync` when available (falls back to `cp -a` if false or absent).
    pub use_rsync: bool,
    /// Verify file checksums during sync (rsync `--checksum`).
    pub checksum_verify: bool,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            exclusions: default_exclusions(),
            use_rsync: true,
            checksum_verify: false,
        }
    }
}

/// `[schedule]` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    /// `systemd` | `cron` | `none`.
    pub method: String,
    /// `hourly` | `daily` | `weekly`.
    pub interval: String,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            method: "systemd".into(),
            interval: "daily".into(),
        }
    }
}

/// Custom or overridden agent definition entry from `[agents.<key>]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomAgentConfig {
    /// Whether this agent is enabled.
    pub enabled: bool,
    /// Display name override.
    pub display_name: Option<String>,
    /// List of source locations (paths). Each may use `~`.
    pub locations: Vec<String>,
    /// Per-agent extra exclusions.
    pub exclusions: Vec<String>,
}

/// The default exclusion patterns applied to every agent.
///
/// The default exclusion patterns applied to every agent.
///
/// Covers common ephemeral or large directories produced by AI coding agents
/// (logs, sessions, node_modules, temp artifacts) that are not user-relevant
/// configuration data.
pub fn default_exclusions() -> Vec<String> {
    vec![
        // OS / editor junk
        ".DS_Store".into(),
        "Thumbs.db".into(),
        // Temp / swap files
        "*.tmp".into(),
        "*.temp".into(),
        "*.swp".into(),
        "*~".into(),
        "*.bak".into(),
        // Logs (often gigabytes in practice, e.g. Codex's codex-tui.log)
        "*.log".into(),
        "**/log/**".into(),
        "**/logs/**".into(),
        "**/Log/**".into(),
        // Cache directories
        "**/cache/**".into(),
        "**/Cache/**".into(),
        "**/.cache/**".into(),
        "**/paste-cache/**".into(),
        // SQLite databases
        "*.sqlite".into(),
        "*.sqlite3".into(),
        "*.sqlite3-wal".into(),
        "*.sqlite3-shm".into(),
        // Session / replay / timeline data (not configuration)
        "**/sessions/**".into(),
        "**/logseq/**".into(),
        "**/history/**".into(),
        "**/history.jsonl".into(),
        "**/session_index.jsonl".into(),
        "**/files-history/**".into(),
        // Claude Code specific (LARGE - not user config)
        "**/projects/**".into(),
        "**/transcripts/**".into(),
        "**/plans/**".into(),
        "**/skill-learning/**".into(),
        "**/*.jsonl".into(),
        "**/tasks/**".into(),
        // Memories (Codex etc - AI-generated summaries, not config)
        "**/memories/**".into(),
        // Shell snapshots
        "**/shell_snapshots/**".into(),
        "**/shell_snapshot/**".into(),
        // Downloads (if any exist in agent dirs)
        "**/downloads/**".into(),
        // Python/JS dependencies (never config)
        "**/node_modules/**".into(),
        "**/__pycache__/**".into(),
        "**/.venv/**".into(),
        "**/venv/**".into(),
        // Blob storage directories (OpenCode etc)
        "**/storage/**".into(),
        // Agent temp / workdirs
        "**/tmp/**".into(),
        "**/temp/**".into(),
    ]
}

impl Config {
    /// Resolve the config path, honouring `CASB_CONFIG` if set.
    pub fn config_path() -> Result<PathBuf> {
        if let Ok(env) = std::env::var("CASB_CONFIG") {
            return Ok(expand_tilde(env));
        }
        let home = home_dir()?;
        Ok(home.join(".config").join("casb").join("config.toml"))
    }

    /// Load config from disk, falling back to defaults if the file is missing.
    /// Then apply environment-variable overrides.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(Some(&path))
    }

    /// Load from an explicit path (or default if `None`). Missing files
    /// produce default values rather than errors.
    pub fn load_from(path: Option<&Path>) -> Result<Self> {
        let mut cfg = match path {
            Some(p) if p.exists() => {
                let text = std::fs::read_to_string(p)?;
                toml::from_str::<Self>(&text)?
            }
            _ => Self::default(),
        };
        cfg.apply_env_overrides();
        Ok(cfg)
    }

    /// Apply environment-variable overrides in place.
    pub fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("CASB_BACKUP_ROOT") {
            self.general.backup_root = v;
        }
        if let Ok(v) = std::env::var("CASB_AUTO_COMMIT") {
            self.general.auto_commit = parse_bool(&v).unwrap_or(self.general.auto_commit);
        }
        if let Ok(v) = std::env::var("CASB_VERBOSE") {
            self.general.verbose = parse_bool(&v).unwrap_or(self.general.verbose);
        }
        if let Ok(v) = std::env::var("CASB_OUTPUT_FORMAT") {
            self.general.output_format = v;
        }
    }

    /// Resolve the absolute backup root, expanding `~`.
    pub fn backup_root(&self) -> PathBuf {
        expand_tilde(&self.general.backup_root)
    }

    /// Persist this configuration to disk at the resolved path. Parent
    /// directories are created as needed.
    pub fn save(&self) -> Result<PathBuf> {
        let path = Self::config_path()?;
        self.save_to(&path)?;
        Ok(path)
    }

    /// Persist to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Get a single dotted-key string value (for `casb config get`).
    pub fn get_dotted(&self, key: &str) -> Option<String> {
        match key {
            "general.backup_root" => Some(self.general.backup_root.clone()),
            "general.auto_commit" => Some(self.general.auto_commit.to_string()),
            "general.verbose" => Some(self.general.verbose.to_string()),
            "general.quiet" => Some(self.general.quiet.to_string()),
            "general.output_format" => Some(self.general.output_format.clone()),
            "backup.use_rsync" => Some(self.backup.use_rsync.to_string()),
            "backup.checksum_verify" => Some(self.backup.checksum_verify.to_string()),
            "backup.exclusions" => Some(self.backup.exclusions.join(",")),
            "schedule.method" => Some(self.schedule.method.clone()),
            "schedule.interval" => Some(self.schedule.interval.clone()),
            _ => None,
        }
    }

    /// Set a dotted-key value from a string (for `casb config set`).
    pub fn set_dotted(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "general.backup_root" => self.general.backup_root = value.to_string(),
            "general.auto_commit" => {
                self.general.auto_commit = parse_bool(value).ok_or_else(|| {
                    CasbError::InvalidArgument(format!("expected bool, got '{value}'"))
                })?;
            }
            "general.verbose" => {
                self.general.verbose = parse_bool(value).ok_or_else(|| {
                    CasbError::InvalidArgument(format!("expected bool, got '{value}'"))
                })?;
            }
            "general.quiet" => {
                self.general.quiet = parse_bool(value).ok_or_else(|| {
                    CasbError::InvalidArgument(format!("expected bool, got '{value}'"))
                })?;
            }
            "general.output_format" => self.general.output_format = value.to_string(),
            "backup.use_rsync" => {
                self.backup.use_rsync = parse_bool(value).ok_or_else(|| {
                    CasbError::InvalidArgument(format!("expected bool, got '{value}'"))
                })?;
            }
            "backup.checksum_verify" => {
                self.backup.checksum_verify = parse_bool(value).ok_or_else(|| {
                    CasbError::InvalidArgument(format!("expected bool, got '{value}'"))
                })?;
            }
            "schedule.method" => self.schedule.method = value.to_string(),
            "schedule.interval" => self.schedule.interval = value.to_string(),
            other => {
                return Err(CasbError::InvalidArgument(format!(
                    "unknown config key: {other}"
                )));
            }
        }
        Ok(())
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" | "on" => Some(true),
        "false" | "0" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_round_trip() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg.general.backup_root, back.general.backup_root);
        assert_eq!(cfg.backup.use_rsync, back.backup.use_rsync);
    }

    #[test]
    fn load_missing_returns_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let cfg = Config::load_from(Some(&path)).unwrap();
        assert_eq!(cfg.general.backup_root, "~/.agent_settings_backups");
    }

    #[test]
    fn load_partial_overrides_only_present_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(
            &path,
            r#"
[general]
backup_root = "/tmp/casb-test"
"#,
        )
        .unwrap();
        let cfg = Config::load_from(Some(&path)).unwrap();
        assert_eq!(cfg.general.backup_root, "/tmp/casb-test");
        assert!(cfg.general.auto_commit, "should keep default");
    }

    #[test]
    fn dotted_get_and_set() {
        let mut cfg = Config::default();
        cfg.set_dotted("general.verbose", "true").unwrap();
        assert_eq!(cfg.get_dotted("general.verbose").as_deref(), Some("true"));
        assert!(cfg.set_dotted("general.bogus", "x").is_err());
        assert!(cfg.set_dotted("general.verbose", "maybe").is_err());
    }

    #[test]
    fn save_and_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.toml");
        let mut cfg = Config::default();
        cfg.general.backup_root = "/tmp/x".into();
        cfg.save_to(&path).unwrap();
        let back = Config::load_from(Some(&path)).unwrap();
        assert_eq!(back.general.backup_root, "/tmp/x");
    }
}
