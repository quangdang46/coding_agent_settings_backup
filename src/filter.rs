//! Smart file-exclusion filtering.
//!
//! Combines default exclusion patterns from [`crate::config::default_exclusions`]
//! with config-level extras and per-agent `.casbignore` files. Patterns
//! follow gitignore semantics via the `glob` crate.

use crate::error::Result;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

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
#[derive(Clone)]
pub struct ExclusionFilter {
    /// Non-literal patterns, evaluated with `glob::Pattern::matches_with`.
    patterns: Vec<glob::Pattern>,
    /// Literal patterns (no glob metacharacters) kept as plain strings so
    /// they resolve via `HashSet` lookup instead of a glob evaluation.
    literal: HashSet<String>,
    raw: Vec<String>,
    /// Subset of `patterns`/`literal` that match credential-shaped filenames
    /// (`.credentials.json`, `auth.json`, `*.pem`, `.env*`, …). These are
    /// ordinary exclusions for sync purposes but are tracked separately so
    /// the backup can warn about *which* skipped files look like secrets.
    secret_patterns: Vec<glob::Pattern>,
    secret_literal: HashSet<String>,
    /// Maximum size, in bytes, of a single file that may be backed up.
    /// `None` disables the cap.
    max_file_size: Option<u64>,
}

impl ExclusionFilter {
    /// Build a filter from a flat list of patterns.
    pub fn new(patterns: Vec<String>) -> Result<Self> {
        let mut filter = Self {
            patterns: Vec::with_capacity(patterns.len()),
            literal: HashSet::new(),
            raw: Vec::with_capacity(patterns.len()),
            secret_patterns: Vec::new(),
            secret_literal: HashSet::new(),
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
            if is_literal_pattern(&variant) {
                self.literal.insert(variant.clone());
            } else {
                self.patterns.push(glob::Pattern::new(&variant)?);
            }
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
            // symlink_metadata, not metadata: rsync's --max-size measures
            // the link itself, and following the link would both mis-size
            // symlinks and fail on dangling ones.
            Some(_) => std::fs::symlink_metadata(abs_path)
                .map(|m| self.exceeds_size_cap_meta(&m))
                .unwrap_or(false),
        }
    }

    /// [`ExclusionFilter::exceeds_size_cap`] for callers that already hold
    /// the entry's metadata (e.g. a `walkdir::DirEntry`), avoiding a
    /// redundant `stat` per file.
    pub fn exceeds_size_cap_meta(&self, meta: &std::fs::Metadata) -> bool {
        match self.max_file_size {
            None => false,
            Some(cap) => meta.is_file() && meta.len() > cap,
        }
    }

    /// Add credential-shaped patterns as ordinary exclusions AND record
    /// them in `secret_patterns` so callers can identify secret-looking
    /// skips. Used when `backup.include_secrets` is false (the default).
    ///
    /// A leading `**/` does not reliably match the location root itself,
    /// so every `**/name` pattern also contributes a bare `name` variant —
    /// a secret sitting directly in `~/.codex/auth.json` must be caught.
    pub fn add_secret_patterns(&mut self, patterns: &[String]) -> Result<()> {
        for p in patterns {
            let mut variants = pattern_variants(p);
            if let Some(stripped) = p.strip_prefix("**/") {
                if !stripped.is_empty() {
                    variants.push(stripped.to_string());
                }
            }
            for variant in &variants {
                if is_literal_pattern(variant) {
                    self.secret_literal.insert(variant.clone());
                } else {
                    self.secret_patterns.push(glob::Pattern::new(variant)?);
                }
                self.push_pattern(variant)?;
            }
        }
        Ok(())
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
    ///
    /// The file is read as bytes and decoded lossily: a `.casbignore`
    /// containing non-UTF8 bytes must not abort the whole agent backup —
    /// invalid sequences become U+FFFD and simply match nothing.
    pub fn merge_casbignore_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
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
        matches_filter(&self.patterns, &self.literal, rel_path)
    }

    /// Whether `rel_path` matches one of the credential-shaped patterns —
    /// i.e. it is excluded *because it looks like a secret*, not because of
    /// an ordinary exclusion. Only meaningful when secret patterns were
    /// added via [`ExclusionFilter::add_secret_patterns`].
    pub fn is_secret_excluded(&self, rel_path: &Path) -> bool {
        matches_filter(&self.secret_patterns, &self.secret_literal, rel_path)
    }

    /// The compiled patterns as their raw string form. Useful for `.gitignore`
    /// generation in the backup repo.
    pub fn raw_patterns(&self) -> &[String] {
        &self.raw
    }
}

/// Whether a pattern contains no glob metacharacters and can therefore be
/// matched by plain string equality against the path candidates.
fn is_literal_pattern(pattern: &str) -> bool {
    !pattern.contains(['*', '?', '[', ']', '{', '}', '\\'])
}

/// The candidate strings a relative path is tested against: the full
/// path, its basename, and every intermediate component — computed ONCE
/// per call instead of once per pattern per component.
struct Candidates<'a> {
    path: std::borrow::Cow<'a, str>,
    basename: std::borrow::Cow<'a, str>,
    components: Vec<std::borrow::Cow<'a, str>>,
}

fn candidates(rel_path: &Path) -> Candidates<'_> {
    let raw = rel_path.to_string_lossy();
    // Normalise to forward slashes for consistent glob matching.
    let path = if cfg!(windows) {
        std::borrow::Cow::Owned(raw.replace('\\', "/"))
    } else {
        raw
    };
    let basename = rel_path
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let components = rel_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    Candidates {
        path,
        basename,
        components,
    }
}

fn debug_exclude() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("CASB_DEBUG_EXCLUDE").is_some())
}

/// Test `rel_path` against a pattern set using the same gitignore-style
/// rules as [`ExclusionFilter::is_excluded`]: the full relative path, its
/// basename, and every intermediate component are tried — first against
/// the literal `HashSet` (O(1) per candidate), then against the compiled
/// glob patterns.
fn matches_filter(patterns: &[glob::Pattern], literals: &HashSet<String>, rel_path: &Path) -> bool {
    let c = candidates(rel_path);
    let debug = debug_exclude();
    for cand in std::iter::once(&c.path)
        .chain(std::iter::once(&c.basename))
        .chain(c.components.iter())
    {
        if literals.contains(cand.as_ref()) {
            if debug {
                eprintln!(
                    "[exclude DEBUG] '{}' matched literal pattern '{}'",
                    rel_path.display(),
                    cand
                );
            }
            return true;
        }
    }
    // `require_literal_separator: true` matches gitignore semantics: `*`
    // and `?` never cross `/`. With `false`, a pattern like `cache/*.txt`
    // also matched `cache/sub/dir.txt` — over-excluding versus the
    // `.gitignore` written into the backup repo. `**` still spans
    // separators, and basename/component matching covers the
    // no-slash-pattern case.
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    for pat in patterns {
        if pat.matches_with(&c.path, opts) || pat.matches_with(&c.basename, opts) {
            if debug {
                eprintln!(
                    "[exclude DEBUG] '{}' matched path/basename '{}' with pattern '{}'",
                    rel_path.display(),
                    c.path,
                    pat
                );
            }
            return true;
        }
        // For ** trailing patterns like `**/cache/**`, match if any
        // intermediate component matches the literal segment.
        for component in &c.components {
            if pat.matches_with(component, opts) {
                if debug {
                    eprintln!(
                        "[exclude DEBUG] '{}' matched component '{}' with pattern '{}'",
                        rel_path.display(),
                        component,
                        pat
                    );
                }
                return true;
            }
        }
    }
    false
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

    /// Regression (bead secrets-committed-plaintext-fqa): credential-shaped
    /// patterns must match at the location root AND nested, and must be
    /// distinguishable from ordinary exclusions.
    #[test]
    fn secret_patterns_match_root_and_nested() {
        let mut f = ExclusionFilter::new(vec![]).unwrap();
        f.add_secret_patterns(&crate::config::default_secret_patterns())
            .unwrap();
        for p in [
            ".credentials.json",
            "auth.json",
            "sub/dir/auth.json",
            "server.pem",
            "deep/nested/cert.p12",
            ".env",
            ".env.local",
            "config/.env.production",
            "id_rsa",
            "nested/id_ed25519",
            ".netrc",
            "secrets.yaml",
        ] {
            let path = PathBuf::from(p);
            assert!(f.is_excluded(&path), "{p} must be excluded");
            assert!(f.is_secret_excluded(&path), "{p} must be secret-flagged");
        }
        // Ordinary files are neither excluded nor secret-flagged.
        let normal = PathBuf::from("settings.json");
        assert!(!f.is_excluded(&normal));
        assert!(!f.is_secret_excluded(&normal));
        // `*.key` must not swallow `monkey.txt` or `keynote.md`.
        assert!(!f.is_excluded(&PathBuf::from("monkey.txt")));
        assert!(!f.is_excluded(&PathBuf::from("keyboard.json")));
    }

    /// Regression (bead rwj): a `.casbignore` with non-UTF8 bytes must not
    /// fail the whole agent backup — decode lossily instead.
    #[test]
    fn casbignore_non_utf8_does_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".casbignore");
        let mut bytes = b"*.log\n".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        bytes.extend_from_slice(b"secret.bin\n");
        std::fs::write(&path, &bytes).unwrap();
        let mut f = ExclusionFilter::new(vec![]).unwrap();
        f.merge_casbignore_file(&path).unwrap();
        assert!(f.is_excluded(&PathBuf::from("a.log")));
        assert!(f.is_excluded(&PathBuf::from("secret.bin")));
    }
}
