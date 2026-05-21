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

    /// Initialize a new git repository at `self.path`. Creates the
    /// directory (and intermediate dirs) if missing. The default branch is
    /// `main`. Idempotent.
    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.path)?;
        if self.exists() {
            return Ok(());
        }
        run_git_in(&self.path, &["init", "-q", "-b", "main"])?;
        Ok(())
    }

    /// Stage all current working tree changes.
    pub fn add_all(&self) -> Result<()> {
        run_git_in(&self.path, &["add", "-A"])?;
        Ok(())
    }

    /// Commit the staged changes with `message`. If there is nothing to
    /// commit, returns `Ok(false)` (no-op). Returns `Ok(true)` on success.
    pub fn commit(&self, message: &str) -> Result<bool> {
        // `git diff --cached --quiet` exits 0 when no staged changes, 1 otherwise.
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(["diff", "--cached", "--quiet"])
            .output()?;
        if out.status.success() {
            return Ok(false); // nothing to commit
        }
        run_git_in(
            &self.path,
            &[
                "-c",
                "user.email=casb@local",
                "-c",
                "user.name=casb",
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
        match message {
            Some(msg) => {
                run_git_in(
                    &self.path,
                    &[
                        "-c",
                        "user.email=casb@local",
                        "-c",
                        "user.name=casb",
                        "tag",
                        "-a",
                        name,
                        "-m",
                        msg,
                        "-f",
                    ],
                )?;
            }
            None => {
                run_git_in(&self.path, &["tag", name, "-f"])?;
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
        let limit_arg = format!("-n{limit}");
        let out = Command::new("git")
            .current_dir(&self.path)
            .args([
                "log",
                "--pretty=format:%H%x1f%h%x1f%aI%x1f%an%x1f%s",
                &limit_arg,
            ])
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
