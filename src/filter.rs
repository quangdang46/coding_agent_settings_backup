//! Smart file-exclusion filtering.
//!
//! Combines default exclusion patterns from [`crate::config::default_exclusions`]
//! with config-level extras and per-agent `.casbignore` files. Patterns
//! follow gitignore semantics via the `glob` crate.

use crate::error::Result;
use std::path::Path;

/// Compiled exclusion ruleset for one backup operation.
pub struct ExclusionFilter {
    patterns: Vec<glob::Pattern>,
    raw: Vec<String>,
}

impl ExclusionFilter {
    /// Build a filter from a flat list of patterns.
    pub fn new(patterns: Vec<String>) -> Result<Self> {
        let mut compiled = Vec::with_capacity(patterns.len());
        for p in &patterns {
            compiled.push(glob::Pattern::new(p)?);
        }
        Ok(Self {
            patterns: compiled,
            raw: patterns,
        })
    }

    /// Build the canonical filter for `casb`: defaults + config extras +
    /// per-agent extras (from `[agents.<key>].exclusions`).
    pub fn from_layers(
        defaults: &[String],
        config_extras: &[String],
        agent_extras: &[String],
    ) -> Result<Self> {
        let mut combined =
            Vec::with_capacity(defaults.len() + config_extras.len() + agent_extras.len());
        combined.extend(defaults.iter().cloned());
        combined.extend(config_extras.iter().cloned());
        combined.extend(agent_extras.iter().cloned());
        Self::new(combined)
    }

    /// Augment with patterns parsed from a `.casbignore` file. Lines that
    /// are empty or start with `#` are ignored. A trailing `/` marks the
    /// pattern as directory-only: it is added as both `name` and `name/**`.
    pub fn merge_casbignore_text(&mut self, text: &str) -> Result<()> {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (base, dir_only) = match trimmed.strip_suffix('/') {
                Some(rest) => (rest, true),
                None => (trimmed, false),
            };
            self.patterns.push(glob::Pattern::new(base)?);
            self.raw.push(base.to_string());
            if dir_only {
                let nested = format!("{base}/**");
                self.patterns.push(glob::Pattern::new(&nested)?);
                self.raw.push(nested);
            }
        }
        Ok(())
    }

    /// Read a `.casbignore` file from disk and merge its patterns. Missing
    /// files are silently ignored.
    pub fn merge_casbignore_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let text = std::fs::read_to_string(path)?;
        self.merge_casbignore_text(&text)
    }

    /// Test whether a relative path is excluded.
    ///
    /// Each pattern is tested both against the full relative path and
    /// against the file/component basename (gitignore-style). Patterns
    /// containing `**` are matched with `MatchOptions::default()`, which
    /// honours `**` only at separator boundaries.
    pub fn is_excluded(&self, rel_path: &Path) -> bool {
        let path_str = rel_path.to_string_lossy();
        let basename = rel_path
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        let opts = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        for pat in &self.patterns {
            if pat.matches_with(&path_str, opts) || pat.matches_with(&basename, opts) {
                return true;
            }
            // For ** trailing patterns like `**/cache/**`, match if any
            // intermediate component matches the literal segment.
            for component in rel_path.components() {
                let cs = component.as_os_str().to_string_lossy();
                if pat.matches_with(&cs, opts) {
                    return true;
                }
            }
        }
        false
    }

    /// The compiled patterns as their raw string form. Useful for `.gitignore`
    /// generation in the backup repo.
    pub fn raw_patterns(&self) -> &[String] {
        &self.raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn excludes_log_files() {
        let f = ExclusionFilter::new(vec!["*.log".into()]).unwrap();
        assert!(f.is_excluded(&PathBuf::from("debug.log")));
        assert!(f.is_excluded(&PathBuf::from("dir/debug.log")));
        assert!(!f.is_excluded(&PathBuf::from("debug.txt")));
    }

    #[test]
    fn excludes_cache_directories() {
        let f = ExclusionFilter::new(vec!["**/cache/**".into(), "**/cache".into()]).unwrap();
        assert!(f.is_excluded(&PathBuf::from("foo/cache/bar")));
        assert!(f.is_excluded(&PathBuf::from("cache/bar")));
    }

    #[test]
    fn keeps_sqlite_dbs_excludes_wal() {
        let f = ExclusionFilter::new(vec!["*.sqlite3-wal".into(), "*.sqlite3-shm".into()]).unwrap();
        assert!(!f.is_excluded(&PathBuf::from("opencode.db")));
        assert!(!f.is_excluded(&PathBuf::from("logs_2.sqlite")));
        assert!(f.is_excluded(&PathBuf::from("opencode.sqlite3-wal")));
    }

    #[test]
    fn merges_casbignore_text() {
        let mut f = ExclusionFilter::new(vec![]).unwrap();
        f.merge_casbignore_text("# comment\n\n*.tmp\nbig/\n")
            .unwrap();
        assert!(f.is_excluded(&PathBuf::from("foo.tmp")));
        assert!(f.is_excluded(&PathBuf::from("big")));
    }

    #[test]
    fn from_layers_combines_all() {
        let f = ExclusionFilter::from_layers(
            &["*.log".to_string()],
            &["secret.json".to_string()],
            &["**/private/**".to_string()],
        )
        .unwrap();
        assert!(f.is_excluded(&PathBuf::from("a.log")));
        assert!(f.is_excluded(&PathBuf::from("secret.json")));
        assert!(f.is_excluded(&PathBuf::from("dir/private/x")));
    }
}
