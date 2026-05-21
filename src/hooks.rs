//! Hook execution system.
//!
//! Hooks are executable scripts placed in
//! `~/.config/casb/<kind>.d/` and run alphabetically before/after
//! backup/restore operations. They receive the agent key as the first
//! positional argument plus a few environment variables.

use crate::error::{CasbError, Result};
use crate::util::home_dir;
use std::path::PathBuf;
use std::process::Command;

/// Hook lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookKind {
    /// Before the backup sync.
    PreBackup,
    /// After the backup sync.
    PostBackup,
    /// Before restore confirmation/sync.
    PreRestore,
    /// After restore sync.
    PostRestore,
}

impl HookKind {
    /// Directory name (relative to the hooks root).
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::PreBackup => "pre-backup.d",
            Self::PostBackup => "post-backup.d",
            Self::PreRestore => "pre-restore.d",
            Self::PostRestore => "post-restore.d",
        }
    }
}

/// All four kinds, in declaration order.
pub fn all_kinds() -> [HookKind; 4] {
    [
        HookKind::PreBackup,
        HookKind::PostBackup,
        HookKind::PreRestore,
        HookKind::PostRestore,
    ]
}

/// Resolve `~/.config/casb/`.
pub fn hooks_root() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(home.join(".config").join("casb"))
}

/// Resolve `~/.config/casb/<kind>.d/`.
pub fn hooks_dir(kind: HookKind) -> Result<PathBuf> {
    Ok(hooks_root()?.join(kind.dir_name()))
}

/// One hook script entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HookEntry {
    /// Phase.
    pub kind: HookKind,
    /// Path to the executable.
    pub path: PathBuf,
    /// Whether the file is executable.
    pub executable: bool,
}

/// Enumerate every configured hook script.
pub fn list_all() -> Result<Vec<HookEntry>> {
    let mut out = Vec::new();
    for kind in all_kinds() {
        let dir = hooks_dir(kind)?;
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let executable = is_executable(&path);
            out.push(HookEntry {
                kind,
                path,
                executable,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Run every executable hook script for `kind` in alphabetical order.
///
/// Failures from individual scripts are reported as [`CasbError::CommandFailed`]
/// and abort the run.
pub fn run_hooks(kind: HookKind, agent_key: &str) -> Result<()> {
    let dir = hooks_dir(kind)?;
    if !dir.exists() {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    for path in entries {
        if !is_executable(&path) {
            continue;
        }
        let mut cmd = Command::new(&path);
        cmd.arg(agent_key)
            .env("CASB_HOOK", kind.dir_name())
            .env("CASB_AGENT", agent_key);
        let status = cmd.status()?;
        if !status.success() {
            return Err(CasbError::CommandFailed {
                command: path.display().to_string(),
                stderr: format!("hook exited with status {status}"),
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_names_unique() {
        let mut names: Vec<&str> = all_kinds().iter().map(|k| k.dir_name()).collect();
        names.sort();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len());
    }
}
