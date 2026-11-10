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
    /// Largest single file (in MiB) that may be backed up.
    ///
    /// Agent directories routinely contain reinstallable binaries — macOS
    /// `.app` bundles, downloaded CLI versions, model blobs — that would
    /// otherwise be committed to the backup repo and bloat it by hundreds of
    /// megabytes. Files above this limit are skipped. `0` disables the cap.
    pub max_file_size_mb: u64,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            exclusions: default_exclusions(),
            use_rsync: true,
            checksum_verify: false,
            max_file_size_mb: 10,
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
        // ── OS / editor junk ──────────────────────────────────────
        ".DS_Store".into(),
        "Thumbs.db".into(),
        // ── Temp / swap files ─────────────────────────────────────
        "*.tmp".into(),
        "*.temp".into(),
        "*.swp".into(),
        "*~".into(),
        "*.bak".into(),
        // ── Logs ──────────────────────────────────────────────────
        "*.log".into(),
        "**/log/**".into(),
        "**/logs/**".into(),
        "**/Log/**".into(),
        // ── Cache directories ─────────────────────────────────────
        "**/cache/**".into(),
        "**/Cache/**".into(),
        "**/.cache/**".into(),
        "**/paste-cache/**".into(),
        // ── SQLite databases (state, not config) ──────────────────
        "*.sqlite".into(),
        "*.sqlite3".into(),
        "*.sqlite3-wal".into(),
        "*.sqlite3-shm".into(),
        "*.sqlite-shm".into(),
        "*.sqlite-wal".into(),
        // ── Session / replay / timeline data ──────────────────────
        "**/sessions/**".into(),
        "**/logseq/**".into(),
        "**/history/**".into(),
        "**/history.jsonl".into(),
        "**/session_index.jsonl".into(),
        "**/files-history/**".into(),
        "**/session-env/**".into(),
        // ── Claude Code specific (LARGE - not user config) ────────
        "**/projects/**".into(),
        "projects/".into(), // rsync-compatible variant
        "**/transcripts/**".into(),
        "**/plans/**".into(),
        "**/skill-learning/**".into(),
        "**/*.jsonl".into(),
        "**/tasks/**".into(),
        "**/file-history/**".into(),
        "**/backups/**".into(),
        "**/shell_snapshots/**".into(),
        "**/shell-snapshots/**".into(),
        "**/shell_snapshot/**".into(),
        "**/telemetry/**".into(),
        "**/downloads/**".into(),
        // Skills from marketplace (can be reinstalled)
        "**/skills/**".into(),
        // Plugin marketplace data (can be reinstalled)
        "**/marketplaces/**".into(),
        // ── Codex specific ────────────────────────────────────────
        "**/packages/**".into(),
        "packages/".into(), // rsync-compatible variant
        "**/models_cache.json".into(),
        "**/vendor_imports/**".into(),
        "**/computer-use/**".into(),
        "computer-use/".into(), // rsync-compatible variant
        "**/.tmp/**".into(),
        // ── Cursor specific (reinstallable skills) ────────────────
        "**/skills-cursor/**".into(),
        "skills-cursor/".into(), // rsync-compatible variant
        // ── OpenCode specific ─────────────────────────────────────
        "**/node_modules/**".into(),
        "**/package-lock.json".into(),
        "**/package.json".into(),
        "**/snapshot/**".into(),
        "**/tool-output/**".into(),
        "*.db-shm".into(),
        "*.db-wal".into(),
        // ── Python ────────────────────────────────────────────────
        "**/__pycache__/**".into(),
        "**/.venv/**".into(),
        "**/venv/**".into(),
        // ── Blob storage (OpenCode etc) ───────────────────────────
        "**/storage/**".into(),
        // ── Agent temp / workdirs ─────────────────────────────────
        "**/tmp/**".into(),
        "**/temp/**".into(),
        // ── Agent runtime / binary artifacts ──────────────────────
        "**/plugins/**".into(),
        "plugins/".into(), // rsync-compatible variant
        // ── Large binary / generated files ────────────────────────
        "*.sqlite".into(),
        "*.db".into(),
        "**/goals_*.sqlite*".into(),
        "**/logs_*.sqlite*".into(),
        "**/memories_*.sqlite*".into(),
        "**/state_*.sqlite*".into(),
        "**/data/versions/**".into(),
        "**/versions/**".into(),
        "versions/".into(), // rsync-compatible variant
        "**/*.app/**".into(),
        "*.app/".into(), // rsync-compatible variant
        "**/dist/**".into(),
        "**/target/**".into(),
        "**/*.bin".into(),
        "**/*.exe".into(),
        "**/*.dll".into(),
        "**/*.dylib".into(),
        "**/*.so".into(),
        "**/*.wasm".into(),
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

    /// Set a single dotted key directly in the TOML file at `path`,
    /// preserving every other byte of the document — unrelated keys,
    /// comments, ordering, formatting.
    ///
    /// Unlike [`Config::save_to`], which serialises the whole *effective*
    /// config, this patches only the requested key. That matters because the
    /// effective config contains environment-resolved values (e.g.
    /// `CASB_BACKUP_ROOT`): re-serialising it would bake those ephemeral
    /// values into the user's real config file.
    ///
    /// The key must already be valid for [`Config::set_dotted`]; the value
    /// literal is typed to match (bool keys get TOML booleans, the size cap
    /// gets an integer, everything else a quoted string).
    pub fn set_key_in_file(path: &Path, key: &str, value: &str) -> Result<()> {
        let literal = toml_literal_for(key, value)?;
        let (section, leaf) = key.split_once('.').ok_or_else(|| {
            CasbError::InvalidArgument(format!("config key must be dotted: {key}"))
        })?;

        let text = if path.exists() {
            std::fs::read_to_string(path)?
        } else {
            String::new()
        };

        let mut out: Vec<String> = Vec::new();
        let mut in_section = false;
        let mut section_seen = false;
        let mut key_written = false;

        for line in text.lines() {
            let trimmed = line.trim();
            // Section header (single-bracket tables only — casb configs
            // never use [[array-of-tables]]). Whitespace inside brackets is
            // tolerated.
            if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.starts_with("[[") {
                let name = trimmed[1..trimmed.len() - 1].trim();
                if in_section && !key_written {
                    // Left the target section without finding the key:
                    // append it at the end of that section.
                    out.push(format!("{leaf} = {literal}"));
                    key_written = true;
                }
                in_section = name == section;
                section_seen |= in_section;
                out.push(line.to_string());
                continue;
            }
            if in_section && !key_written && is_toml_key_line(line, leaf) {
                out.push(format!("{leaf} = {literal}"));
                key_written = true;
                continue;
            }
            out.push(line.to_string());
        }
        if !section_seen {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("[{section}]"));
            out.push(format!("{leaf} = {literal}"));
        } else if !key_written && in_section {
            // Target section was the last section and the key was absent:
            // append at the end of the file (still inside that section).
            out.push(format!("{leaf} = {literal}"));
        }

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut body = out.join("\n");
        body.push('\n');
        std::fs::write(path, body)?;
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
            "backup.max_file_size_mb" => Some(self.backup.max_file_size_mb.to_string()),
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
            "backup.max_file_size_mb" => {
                self.backup.max_file_size_mb = value.trim().parse::<u64>().map_err(|_| {
                    CasbError::InvalidArgument(format!(
                        "expected a non-negative integer, got '{value}'"
                    ))
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

/// Render `value` as a TOML literal appropriate for `key`.
///
/// Mirrors the type coercion in [`Config::set_dotted`]: boolean keys emit
/// TOML booleans, `backup.max_file_size_mb` emits an integer, and every
/// other key emits a quoted basic string.
fn toml_literal_for(key: &str, value: &str) -> Result<String> {
    const BOOL_KEYS: &[&str] = &[
        "general.auto_commit",
        "general.verbose",
        "general.quiet",
        "backup.use_rsync",
        "backup.checksum_verify",
    ];
    if BOOL_KEYS.contains(&key) {
        return parse_bool(value).map(|b| b.to_string()).ok_or_else(|| {
            CasbError::InvalidArgument(format!("expected bool, got '{value}'"))
        });
    }
    if key == "backup.max_file_size_mb" {
        return value
            .trim()
            .parse::<u64>()
            .map(|n| n.to_string())
            .map_err(|_| {
                CasbError::InvalidArgument(format!(
                    "expected a non-negative integer, got '{value}'"
                ))
            });
    }
    Ok(toml_string_literal(value))
}

/// Quote `s` as a TOML basic string with the required escapes.
fn toml_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether `line` assigns to the bare (or quoted) TOML key `leaf`.
fn is_toml_key_line(line: &str, leaf: &str) -> bool {
    let t = line.trim_start();
    for prefix in [leaf.to_string(), format!("\"{leaf}\"")] {
        if let Some(rest) = t.strip_prefix(&prefix) {
            if rest.trim_start().starts_with('=') {
                return true;
            }
        }
    }
    false
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

    #[test]
    fn set_key_in_file_patches_only_that_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.toml");
        let original = r#"# user comment stays
[general]
backup_root = "/original"
auto_commit = true  # trailing comment
verbose = false

[backup]
use_rsync = true
"#;
        std::fs::write(&path, original).unwrap();

        Config::set_key_in_file(&path, "general.verbose", "true").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# user comment stays"));
        assert!(text.contains("backup_root = \"/original\""));
        assert!(text.contains("verbose = true"));
        assert!(text.contains("auto_commit = true  # trailing comment"));
        // Round-trips through the real parser.
        let cfg = Config::load_from(Some(&path)).unwrap();
        assert!(cfg.general.verbose);
        assert_eq!(cfg.general.backup_root, "/original");
    }

    #[test]
    fn set_key_in_file_adds_missing_key_and_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "[general]\nbackup_root = \"/x\"\n").unwrap();

        // Missing key inside an existing section → appended in-section.
        Config::set_key_in_file(&path, "general.quiet", "true").unwrap();
        // Missing section entirely → appended at end.
        Config::set_key_in_file(&path, "schedule.method", "cron").unwrap();
        // Typed literals: bool and integer are not quoted.
        Config::set_key_in_file(&path, "backup.max_file_size_mb", "42").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let cfg = Config::load_from(Some(&path)).unwrap();
        assert!(cfg.general.quiet);
        assert_eq!(cfg.schedule.method, "cron");
        assert_eq!(cfg.backup.max_file_size_mb, 42);
        assert!(text.contains("quiet = true"), "got:\n{text}");
        assert!(text.contains("max_file_size_mb = 42"), "got:\n{text}");
        assert!(text.contains("[schedule]"), "got:\n{text}");
    }

    #[test]
    fn set_key_in_file_creates_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/c.toml");
        Config::set_key_in_file(&path, "general.backup_root", "/tmp/b").unwrap();
        let cfg = Config::load_from(Some(&path)).unwrap();
        assert_eq!(cfg.general.backup_root, "/tmp/b");
    }

    #[test]
    fn set_key_in_file_rejects_bad_typed_values() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "").unwrap();
        assert!(Config::set_key_in_file(&path, "general.verbose", "maybe").is_err());
        assert!(Config::set_key_in_file(&path, "backup.max_file_size_mb", "-1").is_err());
        assert!(Config::set_key_in_file(&path, "nodot", "x").is_err());
    }
}
