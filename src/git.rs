//! Git CLI wrapper.
//!
//! All git operations go through `std::process::Command` to avoid pulling in
//! `libgit2`. Functions take a repository path and return [`crate::Result`].

use crate::error::{CasbError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Handle to a git repository on disk.
#[derive(Debug, Clone)]
pub struct Repo {
    /// Absolute path to the working tree.
    pub path: PathBuf,
}

impl Repo {
    /// Wrap an existing path. Does not validate.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Whether `<path>/.git` exists.
    pub fn exists(&self) -> bool {
        self.path.join(".git").exists()
    }

    /// Whether git can read the repository at `self.path`
    /// (`git rev-parse --git-dir` succeeds). Used to validate archives
    /// before they are allowed to replace the backup root.
    pub fn is_valid_repo(&self) -> bool {
        Command::new("git")
            .current_dir(&self.path)
            .args(["rev-parse", "--git-dir"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Initialize a new git repository at `self.path`. Creates the
    /// directory (and intermediate dirs) if missing. The default branch is
    /// `main`. Idempotent.
    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.path)?;
        if self.exists() {
            return Ok(());
        }
        run_git_in(&self.path, &["init", "-q", "-b", "main"])?;
        self.pin_safe_git_config()?;
        // Disable git's automatic garbage collection. `casb` commits once per
        // agent, and every `git commit` would otherwise spawn a background
        // `git gc --auto`. Those concurrent repacks each write their own pack
        // file, so a single large blob ends up duplicated in one pack per
        // agent and the repo grows by a multiple of its real size. `casb`
        // runs a single controlled `git gc` at the end of a backup instead.
        run_git_in(&self.path, &["config", "gc.auto", "0"])?;
        Ok(())
    }

    /// Pin repository-local config so casb's git operations cannot be
    /// hijacked or broken by the user's *global* git configuration:
    ///
    /// - `core.hooksPath` → an empty directory inside `.git`. Without this,
    ///   a global `core.hooksPath` injects arbitrary hooks into every casb
    ///   commit/checkout/worktree operation.
    /// - `core.fsmonitor` → `false`. A global fsmonitor hook is a command
    ///   git executes on many operations.
    /// - `commit.gpgsign` / `tag.gpgsign` → `false`. Global signing config
    ///   makes every casb commit/tag attempt GPG signing, which fails or
    ///   hangs in non-interactive use.
    ///
    /// Idempotent; safe to call on every command. Local config wins over
    /// global, and the per-command `-c` overrides in [`Repo::commit`] and
    /// [`Repo::tag_create_at`] guard even against a poisoned local config.
    pub fn pin_safe_git_config(&self) -> Result<()> {
        if !self.exists() {
            return Ok(());
        }
        // Only pre-create the directory when `.git` is a real directory;
        // for a gitfile (linked worktree) the path simply won't resolve,
        // which is equally safe — git finds no hooks there.
        let dotgit = self.path.join(".git");
        if dotgit.is_dir() {
            std::fs::create_dir_all(dotgit.join("casb-empty-hooks"))?;
        }
        run_git_in(
            &self.path,
            &["config", "core.hooksPath", ".git/casb-empty-hooks"],
        )?;
        run_git_in(&self.path, &["config", "core.fsmonitor", "false"])?;
        run_git_in(&self.path, &["config", "commit.gpgsign", "false"])?;
        run_git_in(&self.path, &["config", "tag.gpgsign", "false"])?;
        Ok(())
    }

    /// Stage all current working tree changes.
    pub fn add_all(&self) -> Result<()> {
        run_git_in(&self.path, &["add", "-A"])?;
        Ok(())
    }

    /// Stage changes limited to the given paths.
    ///
    /// The backup repo is shared by every agent, so staging the whole tree
    /// for one agent would sweep another agent's files into that agent's
    /// commit. Scoping the staging to the agent's own subdirectories keeps
    /// one agent's commit to that agent's files.
    pub fn add_paths(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        // An agent whose data is entirely excluded has nothing to stage, and
        // `git add` treats a pathspec that matches no file as an error.
        // Skip those agents instead of failing their backup.
        let any_exists = paths
            .iter()
            .any(|p| !p.is_empty() && self.path.join(p).exists());
        if !any_exists {
            return Ok(());
        }
        let mut args = vec!["add", "-A", "--"];
        args.extend(paths.iter().map(|s| s.as_str()));
        match run_git_in(&self.path, &args) {
            Ok(_) => Ok(()),
            // The pathspec matched no file: nothing to stage, not an error.
            Err(CasbError::GitCommand { stderr, .. })
                if stderr.contains("did not match any files") =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// List paths tracked by the index under the given pathspecs
    /// (`git ls-files -- <paths>`). Returns repo-relative paths.
    pub fn ls_files(&self, paths: &[String]) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["ls-files".to_string(), "--".to_string()];
        args.extend(paths.iter().cloned());
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(&args)
            .output()?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// Stop tracking the given paths (`git rm --cached`) while leaving the
    /// working-tree files in place. Used to drop files that have become
    /// excluded (e.g. credential-shaped files) so they leave the index and
    /// the next commit records their removal. `--ignore-unmatch` makes
    /// already-untracked paths a no-op.
    pub fn untrack_paths(&self, paths: &[String]) -> Result<()> {
        const CHUNK: usize = 100;
        for chunk in paths.chunks(CHUNK) {
            if chunk.is_empty() {
                continue;
            }
            let mut args = vec![
                "rm".to_string(),
                "-q".to_string(),
                "--cached".to_string(),
                "--ignore-unmatch".to_string(),
                "--".to_string(),
            ];
            args.extend(chunk.iter().cloned());
            let out = Command::new("git")
                .current_dir(&self.path)
                .args(&args)
                .output()?;
            if !out.status.success() {
                return Err(CasbError::GitCommand {
                    command: format!("git rm --cached -- {}", chunk.len()),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                });
            }
        }
        Ok(())
    }

    /// Like [`Repo::add_paths`] but for paths that may no longer exist on
    /// disk — `git add -A` still stages their deletions from the index.
    /// Used after `prune_stale_subdirs` removes directories.
    pub fn add_paths_allow_missing(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = vec!["add", "-A", "--"];
        args.extend(paths.iter().map(|s| s.as_str()));
        match run_git_in(&self.path, &args) {
            Ok(_) => Ok(()),
            Err(CasbError::GitCommand { stderr, .. })
                if stderr.contains("did not match any files") =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Repack the repository, letting git's own heuristics decide whether
    /// a repack is actually needed. A full `git gc` on every backup is
    /// wasteful on large repos; `gc --auto` exits quickly when the loose-
    /// object/pack thresholds aren't met. `gc.auto=0` (set at init) only
    /// disables *unattended* auto-gc — an explicit `gc --auto` still
    /// evaluates and repacks when warranted.
    ///
    /// Best-effort: a failure here never invalidates the backup itself.
    pub fn compact(&self) -> Result<()> {
        let _ = run_git_in(&self.path, &["gc", "--auto", "--quiet"]);
        Ok(())
    }

    /// Commit the staged changes with `message`. If there is nothing to
    /// commit, returns `Ok(false)` (no-op). Returns `Ok(true)` on success.
    pub fn commit(&self, message: &str) -> Result<bool> {
        // `git diff --cached --quiet` exits 0 when no staged changes, 1 when
        // there are staged changes, and >1 (typically 128) on real errors
        // such as a corrupt index — which must NOT be mistaken for "has
        // changes" and sent to `git commit`.
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(["diff", "--cached", "--quiet"])
            .output()?;
        match out.status.code() {
            Some(0) => return Ok(false), // nothing to commit
            Some(1) => {}                // staged changes exist — commit below
            _ => {
                return Err(CasbError::GitCommand {
                    command: "git diff --cached --quiet".to_string(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                });
            }
        }
        run_git_in(
            &self.path,
            &[
                "-c",
                "user.email=casb@local",
                "-c",
                "user.name=casb",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                message,
            ],
        )?;
        Ok(true)
    }

    /// Force-create a tag at HEAD with optional annotated message.
    pub fn tag_create(&self, name: &str, message: Option<&str>) -> Result<()> {
        self.tag_create_at(name, message, "HEAD")
    }

    /// Force-create a tag at an explicit target (commit-ish) with an
    /// optional annotated message.
    pub fn tag_create_at(&self, name: &str, message: Option<&str>, target: &str) -> Result<()> {
        match message {
            Some(msg) => {
                run_git_in(
                    &self.path,
                    &[
                        "-c",
                        "user.email=casb@local",
                        "-c",
                        "user.name=casb",
                        "-c",
                        "tag.gpgsign=false",
                        "tag",
                        "-a",
                        name,
                        "-m",
                        msg,
                        "-f",
                        target,
                    ],
                )?;
            }
            None => {
                run_git_in(
                    &self.path,
                    &["-c", "tag.gpgsign=false", "tag", name, "-f", target],
                )?;
            }
        }
        Ok(())
    }

    /// List tags.
    pub fn tag_list(&self) -> Result<Vec<String>> {
        let out = run_git_in(&self.path, &["tag", "--sort=-creatordate"])?;
        Ok(out
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// Delete a tag.
    pub fn tag_delete(&self, name: &str) -> Result<()> {
        run_git_in(&self.path, &["tag", "-d", name])?;
        Ok(())
    }

    /// Whether a given ref (commit hash, tag, branch) resolves.
    pub fn ref_exists(&self, refname: &str) -> bool {
        Command::new("git")
            .current_dir(&self.path)
            .args(["rev-parse", "--verify", "--quiet", refname])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get the short HEAD commit hash, or `None` if no commits exist.
    pub fn head_short(&self) -> Result<Option<String>> {
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(["rev-parse", "--short", "HEAD"])
            .output()?;
        if !out.status.success() {
            return Ok(None);
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            Ok(None)
        } else {
            Ok(Some(s))
        }
    }

    /// Number of commits reachable from HEAD. Returns 0 if HEAD is unborn.
    pub fn commit_count(&self) -> Result<u64> {
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(["rev-list", "--count", "HEAD"])
            .output()?;
        if !out.status.success() {
            return Ok(0);
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(s.parse().unwrap_or(0))
    }

    /// Run `git log` and return parsed entries, newest first.
    pub fn log(&self, limit: usize) -> Result<Vec<LogEntry>> {
        self.log_for_paths(limit, &[])
    }

    /// Run `git log` restricted to the given paths.
    ///
    /// All agents share one repository, so an unscoped `git log` returns every
    /// agent's commits. Passing the agent's own subdirectories restricts the
    /// history to the commits that actually touched that agent.
    pub fn log_for_paths(&self, limit: usize, paths: &[String]) -> Result<Vec<LogEntry>> {
        let limit_arg = format!("-n{limit}");
        let mut args = vec![
            "log".to_string(),
            "--pretty=format:%H%x1f%h%x1f%aI%x1f%an%x1f%s".to_string(),
            limit_arg,
        ];
        if !paths.is_empty() {
            args.push("--".to_string());
            args.extend(paths.iter().cloned());
        }
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(&args)
            .output()?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut entries = Vec::new();
        for line in stdout.lines() {
            let parts: Vec<&str> = line.splitn(5, '\u{001f}').collect();
            if parts.len() != 5 {
                continue;
            }
            entries.push(LogEntry {
                hash: parts[0].to_string(),
                short_hash: parts[1].to_string(),
                date: parts[2].to_string(),
                author: parts[3].to_string(),
                subject: parts[4].to_string(),
                tags: Vec::new(),
            });
        }
        // Attach tag names to each commit.
        if let Ok(tag_pairs) = self.tag_pairs() {
            for entry in entries.iter_mut() {
                for (tag, hash) in &tag_pairs {
                    if hash == &entry.hash {
                        entry.tags.push(tag.clone());
                    }
                }
            }
        }
        Ok(entries)
    }

    /// Number of commits that touched the given paths.
    pub fn commit_count_for_paths(&self, paths: &[String]) -> Result<u64> {
        if paths.is_empty() {
            return self.commit_count();
        }
        let mut args = vec![
            "rev-list".to_string(),
            "--count".to_string(),
            "HEAD".to_string(),
        ];
        args.push("--".to_string());
        args.extend(paths.iter().cloned());
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(&args)
            .output()?;
        if !out.status.success() {
            return Ok(0);
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(s.parse().unwrap_or(0))
    }

    /// The short hash of the most recent commit that touched the given paths.
    pub fn last_commit_for_paths(&self, paths: &[String]) -> Result<Option<String>> {
        if paths.is_empty() {
            return self.head_short();
        }
        let mut args = vec![
            "log".to_string(),
            "-1".to_string(),
            "--pretty=format:%h".to_string(),
        ];
        args.push("--".to_string());
        args.extend(paths.iter().cloned());
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(&args)
            .output()?;
        if !out.status.success() {
            return Ok(None);
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            Ok(None)
        } else {
            Ok(Some(s))
        }
    }

    /// `(tag_name, target_commit_hash)` pairs.
    pub fn tag_pairs(&self) -> Result<Vec<(String, String)>> {
        let out = Command::new("git")
            .current_dir(&self.path)
            .args([
                "for-each-ref",
                "--format=%(refname:short)\t%(*objectname)\t%(objectname)",
                "refs/tags",
            ])
            .output()?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        let mut pairs = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.is_empty() {
                continue;
            }
            let name = parts[0].to_string();
            let target = if parts.len() >= 2 && !parts[1].is_empty() {
                parts[1].to_string()
            } else if parts.len() >= 3 {
                parts[2].to_string()
            } else {
                continue;
            };
            pairs.push((name, target));
        }
        Ok(pairs)
    }

    /// Run `git fsck --full --strict` and return its combined output. The
    /// command fails (Err) only on missing git/IO; integrity issues are
    /// returned as the captured stderr text.
    pub fn fsck(&self) -> Result<FsckResult> {
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(["fsck", "--full", "--strict"])
            .output()?;
        Ok(FsckResult {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Run `git diff --name-status` between two refs. Returns parsed
    /// per-path entries.
    pub fn diff_name_status(&self, from: &str, to: &str) -> Result<Vec<DiffEntry>> {
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(["diff", "--name-status", from, to])
            .output()?;
        if !out.status.success() {
            return Err(CasbError::GitCommand {
                command: format!("git diff --name-status {from} {to}"),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let mut entries = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut parts = line.splitn(2, char::is_whitespace);
            let status = parts.next().unwrap_or("?").trim().to_string();
            let path = parts.next().unwrap_or("").trim().to_string();
            if path.is_empty() {
                continue;
            }
            entries.push(DiffEntry {
                status: parse_status(&status),
                path,
            });
        }
        Ok(entries)
    }

    /// Restore working tree to the contents of `refname` using a hard
    /// checkout. Equivalent to `git checkout <ref> -- .`.
    pub fn checkout_ref_into_worktree(&self, refname: &str) -> Result<()> {
        run_git_in(&self.path, &["checkout", refname, "--", "."])?;
        Ok(())
    }

    /// Remove all files tracked by `.gitignore` from the working tree but
    /// keep them in git history. Prevents rsync-copied, gitignore-excluded
    /// binaries (packages/, plugins/, computer-use/, etc.) from wasting
    /// disk space.
    pub fn clean_ignored(&self) -> Result<()> {
        let _ = run_git_in(&self.path, &["clean", "-fdX"]);
        Ok(())
    }

    /// Path to a temporary worktree at the given ref. Caller must
    /// `worktree_remove` afterwards.
    pub fn worktree_add(&self, target: &Path, refname: &str) -> Result<()> {
        let target_str = target.to_string_lossy().to_string();
        run_git_in(
            &self.path,
            &["worktree", "add", "--detach", &target_str, refname],
        )?;
        Ok(())
    }

    /// Remove a worktree previously added with [`worktree_add`].
    pub fn worktree_remove(&self, target: &Path) -> Result<()> {
        let target_str = target.to_string_lossy().to_string();
        let _ = run_git_in(&self.path, &["worktree", "remove", "--force", &target_str]);
        // Always best-effort delete the directory if still present.
        if target.exists() {
            std::fs::remove_dir_all(target).ok();
        }
        Ok(())
    }
}

/// One entry from `git log`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    /// Full commit SHA-1 hash.
    pub hash: String,
    /// Short commit hash.
    pub short_hash: String,
    /// ISO-8601 author date.
    pub date: String,
    /// Author name.
    pub author: String,
    /// Commit subject line.
    pub subject: String,
    /// Tag names pointing at this commit, if any.
    pub tags: Vec<String>,
}

/// One entry from `git diff --name-status`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffEntry {
    /// Diff status code.
    pub status: DiffStatus,
    /// Affected path (relative to repo root).
    pub path: String,
}

/// Diff status as emitted by `git diff --name-status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffStatus {
    /// File added.
    Added,
    /// File modified.
    Modified,
    /// File deleted.
    Deleted,
    /// File renamed.
    Renamed,
    /// Unknown / unhandled.
    Other,
}

/// Result of `git fsck`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FsckResult {
    /// Whether `git fsck` exited 0.
    pub ok: bool,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr (where issues are typically reported).
    pub stderr: String,
}

fn parse_status(s: &str) -> DiffStatus {
    match s {
        "A" => DiffStatus::Added,
        "M" => DiffStatus::Modified,
        "D" => DiffStatus::Deleted,
        s if s.starts_with('R') => DiffStatus::Renamed,
        _ => DiffStatus::Other,
    }
}

fn run_git_in(path: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(path).args(args).output()?;
    if !out.status.success() {
        return Err(CasbError::GitCommand {
            command: format!("git {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_and_commit() {
        let dir = tempdir().unwrap();
        let repo = Repo::new(dir.path());
        repo.init().unwrap();
        assert!(repo.exists());
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        repo.add_all().unwrap();
        let committed = repo.commit("first").unwrap();
        assert!(committed);
        let hash = repo.head_short().unwrap();
        assert!(hash.is_some());
        assert_eq!(repo.commit_count().unwrap(), 1);
    }

    #[test]
    fn empty_commit_no_op() {
        let dir = tempdir().unwrap();
        let repo = Repo::new(dir.path());
        repo.init().unwrap();
        // Without changes, commit should return false.
        let committed = repo.commit("empty").unwrap();
        assert!(!committed);
    }

    #[test]
    fn tag_create_and_list_and_delete() {
        let dir = tempdir().unwrap();
        let repo = Repo::new(dir.path());
        repo.init().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        repo.add_all().unwrap();
        repo.commit("c").unwrap();
        repo.tag_create("v1", Some("first")).unwrap();
        let tags = repo.tag_list().unwrap();
        assert!(tags.contains(&"v1".to_string()));
        assert!(repo.ref_exists("v1"));
        repo.tag_delete("v1").unwrap();
        assert!(!repo.ref_exists("v1"));
    }

    #[test]
    fn log_returns_entries_with_tags() {
        let dir = tempdir().unwrap();
        let repo = Repo::new(dir.path());
        repo.init().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        repo.add_all().unwrap();
        repo.commit("first").unwrap();
        repo.tag_create("v1", None).unwrap();
        let log = repo.log(10).unwrap();
        assert_eq!(log.len(), 1);
        assert!(log[0].tags.contains(&"v1".to_string()));
        assert_eq!(log[0].subject, "first");
    }

    #[test]
    fn fsck_passes_on_fresh_repo() {
        let dir = tempdir().unwrap();
        let repo = Repo::new(dir.path());
        repo.init().unwrap();
        let res = repo.fsck().unwrap();
        assert!(res.ok);
    }
}
