//! Smart file-exclusion filtering.
//!
//! Combines default exclusion patterns from [`crate::config::default_exclusions`]
//! with config-level extras and per-agent `.casbignore` files. Patterns
//! follow gitignore semantics via the `glob` crate.

use crate::error::Result;
use std::path::Path;

/// The pattern variants implied by `pattern`.
///
/// A pattern such as `**/versions/**` matches the *contents* of a
/// `versions` directory but not the directory itself. The directory still
/// has to be excluded: otherwise it is copied into the backup, git ignores
/// everything inside it, and the post-commit `git clean -fdX` then deletes
/// the whole location directory from the backup tree — taking any sibling
/// content with it.
///
/// The trailing-slash form (`versions/`) is the rsync-compatible spelling of
/// the same idea; macOS rsync strips the leading `**` from `**/versions/**`,
/// which is why both spellings exist in the default list.
fn pattern_variants(pattern: &str) -> Vec<String> {
    let mut out = Vec::new();
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return out;
    }
    out.push(trimmed.to_string());
    if let Some(base) = trimmed.strip_suffix('/') {
        if !base.is_empty() {
            out.push(base.to_string());
            out.push(format!("{base}/**"));
        }
    }
    if let Some(stem) = trimmed.strip_suffix("/**") {
        if !stem.is_empty() {
            out.push(stem.to_string());
        }
    }
    out
}

/// Compiled exclusion ruleset for one backup operation.
pub struct ExclusionFilter {
    patterns: Vec<glob::Pattern>,
    raw: Vec<String>,
    /// Maximum size, in bytes, of a single file that may be backed up.
    /// `None` disables the cap.
    max_file_size: Option<u64>,
}

impl ExclusionFilter {
    /// Build a filter from a flat list of patterns.
    pub fn new(patterns: Vec<String>) -> Result<Self> {
        let mut filter = Self {
            patterns: Vec::with_capacity(patterns.len()),
            raw: Vec::with_capacity(patterns.len()),
            max_file_size: None,
        };
        for p in &patterns {
            filter.push_pattern(p)?;
        }
        Ok(filter)
    }

    /// Add one pattern, together with the directory-name variants it implies.
    fn push_pattern(&mut self, pattern: &str) -> Result<()> {
        for variant in pattern_variants(pattern) {
            if self.raw.iter().any(|r| r == &variant) {
                continue;
            }
            self.patterns.push(glob::Pattern::new(&variant)?);
            self.raw.push(variant);
        }
        Ok(())
    }

    /// Set the maximum size of a single file that may be backed up.
    ///
    /// This is the general safety net against committing multi-hundred-megabyte
    /// agent binaries (macOS `.app` bundles, downloaded CLI versions, model
    /// blobs) that no sane name-based pattern would catch. `None` removes the
    /// cap entirely.
    pub fn with_max_file_size(mut self, bytes: Option<u64>) -> Self {
        self.max_file_size = bytes;
        self
    }

    /// The configured per-file size cap in bytes, if any.
    pub fn max_file_size(&self) -> Option<u64> {
        self.max_file_size
    }

    /// Whether `abs_path` points at a file larger than the configured cap.
    ///
    /// Paths that cannot be stat'ed are never excluded — a missing file is
    /// handled by the caller.
    pub fn exceeds_size_cap(&self, abs_path: &Path) -> bool {
        match self.max_file_size {
            None => false,
            Some(cap) => std::fs::metadata(abs_path)
                .map(|m| m.is_file() && m.len() > cap)
                .unwrap_or(false),
        }
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
    /// pattern as directory-only.
    pub fn merge_casbignore_text(&mut self, text: &str) -> Result<()> {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            self.push_pattern(trimmed)?;
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
    ///
    /// On Windows the path is normalised to forward slashes before
    /// matching so that Unix-style glob patterns (`**/cache/**`) work
    /// regardless of the native separator.
    pub fn is_excluded(&self, rel_path: &Path) -> bool {
        let raw = rel_path.to_string_lossy();
        // Normalise to forward slashes for consistent glob matching.
        let path_str: std::borrow::Cow<'_, str> = if cfg!(windows) {
            std::borrow::Cow::Owned(raw.replace('\\', "/"))
        } else {
            raw
        };
        let basename = rel_path
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        let opts = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        // Track which pattern matched for debugging.
        for pat in &self.patterns {
            if pat.matches_with(&path_str, opts) || pat.matches_with(&basename, opts) {
                if std::env::var("CASB_DEBUG_EXCLUDE").is_ok() {
                    eprintln!(
                        "[exclude DEBUG] '{}' matched path/basename '{}' with pattern '{}'",
                        rel_path.display(),
                        path_str,
                        pat
                    );
                }
                return true;
            }
            // For ** trailing patterns like `**/cache/**`, match if any
            // intermediate component matches the literal segment.
            for component in rel_path.components() {
                let cs = component.as_os_str().to_string_lossy();
                if pat.matches_with(&cs, opts) {
                    if std::env::var("CASB_DEBUG_EXCLUDE").is_ok() {
                        eprintln!(
                            "[exclude DEBUG] '{}' matched component '{}' with pattern '{}'",
                            rel_path.display(),
                            cs,
                            pat
                        );
                    }
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

    /// Regression: a pattern that covers a directory's *contents* must also
    /// cover the directory itself. Otherwise the directory is copied into the
    /// backup, git ignores what is inside it, and the post-commit
    /// `git clean -fdX` deletes the whole location directory.
    #[test]
    fn content_pattern_also_excludes_the_directory() {
        let f = ExclusionFilter::new(vec!["**/versions/**".into()]).unwrap();
        assert!(f.is_excluded(&PathBuf::from("versions")));
        assert!(f.is_excluded(&PathBuf::from("versions/current.json")));
        assert!(f.is_excluded(&PathBuf::from("a/b/versions")));
    }

    /// The rsync-style spelling (`dir/`) must exclude the directory too.
    #[test]
    fn trailing_slash_pattern_excludes_the_directory() {
        let f = ExclusionFilter::new(vec!["versions/".into()]).unwrap();
        assert!(f.is_excluded(&PathBuf::from("versions")));
        assert!(f.is_excluded(&PathBuf::from("versions/current.json")));
    }

    #[test]
    fn app_bundle_directory_is_excluded() {
        let f = ExclusionFilter::new(vec!["**/*.app/**".into()]).unwrap();
        assert!(f.is_excluded(&PathBuf::from("ClaudeCode.app")));
        assert!(f.is_excluded(&PathBuf::from("ClaudeCode.app/Contents/MacOS/claude")));
    }
}
