//! Shared utility helpers used across modules.
//!
//! Path expansion, home-directory resolution, prompt helpers, byte-size
//! formatting, and small wrappers around `std::process::Command`.

use crate::error::{CasbError, Result};
use std::ffi::OsStr;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Expand a leading `~` to the user's home directory.
///
/// Returns the input unchanged if it does not begin with `~`.
pub fn expand_tilde(path: impl AsRef<Path>) -> PathBuf {
    let p = path.as_ref();
    let s = match p.to_str() {
        Some(s) => s,
        None => return p.to_path_buf(),
    };
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    p.to_path_buf()
}

/// Resolve the user's home directory, returning a [`CasbError`] if absent.
pub fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| CasbError::Config("could not resolve $HOME".into()))
}

/// Format a byte count as a human-readable string (`1.2 MB`, `345 KB`, ...).
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Compute total size in bytes of a directory tree.
///
/// Symlinks are not followed. Errors during traversal cause the entry to be
/// skipped silently (matching `du -s` behaviour).
pub fn dir_size(path: impl AsRef<Path>) -> u64 {
    let path = path.as_ref();
    if !path.exists() {
        return 0;
    }
    if path.is_file() {
        return std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Prompt the user with `question`. Returns `Ok(true)` if the user answers
/// `y` or `yes`; `Ok(false)` otherwise. If `force` is true, returns
/// `Ok(true)` without prompting.
pub fn confirm(question: &str, force: bool) -> Result<bool> {
    if force {
        return Ok(true);
    }
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

/// Run a subprocess and return captured [`Output`], propagating any IO
/// failures. The caller is responsible for inspecting `output.status` to
/// decide whether the process succeeded.
pub fn run_capture<S: AsRef<OsStr>>(program: S, args: &[&str]) -> Result<Output> {
    let output = Command::new(program.as_ref()).args(args).output()?;
    Ok(output)
}

/// Run a subprocess and return its captured stdout as a string.
///
/// Fails with [`CasbError::CommandFailed`] on non-zero exit status.
pub fn run_check<S: AsRef<OsStr>>(program: S, args: &[&str]) -> Result<String> {
    let output = run_capture(program.as_ref(), args)?;
    if !output.status.success() {
        let mut argv = vec![program.as_ref().to_string_lossy().into_owned()];
        argv.extend(args.iter().map(|s| (*s).to_owned()));
        return Err(CasbError::CommandFailed {
            command: argv.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Check whether an executable is available on `$PATH`.
///
/// On Windows the bare name is checked first, then common executable
/// extensions (`.exe`, `.cmd`, `.bat`, `.com`) are appended because
/// the Windows shell resolves commands via `PATHEXT` but
/// `Path::is_file()` does not.
pub fn which(program: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(program);
            if candidate.is_file() {
                return true;
            }
            #[cfg(windows)]
            {
                for ext in &[".exe", ".cmd", ".bat", ".com"] {
                    let with_ext = dir.join(format!("{program}{ext}"));
                    if with_ext.is_file() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Make a path absolute relative to the current working directory.
pub fn absolutise(path: impl AsRef<Path>) -> Result<PathBuf> {
    let p = path.as_ref();
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        let cwd = std::env::current_dir()?;
        Ok(cwd.join(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_when_home_exists() {
        if let Some(home) = dirs::home_dir() {
            let expanded = expand_tilde("~/foo");
            assert_eq!(expanded, home.join("foo"));
        }
    }

    #[test]
    fn tilde_alone_expands_to_home() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde("~"), home);
        }
    }

    #[test]
    fn no_tilde_unchanged() {
        assert_eq!(expand_tilde("/tmp/foo"), PathBuf::from("/tmp/foo"));
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn which_finds_sh() {
        // `sh` is only guaranteed to exist on Unix; skip the positive
        // assertion on Windows where it's absent from PATH.
        #[cfg(unix)]
        assert!(which("sh"));
        assert!(!which("definitely-not-a-real-program-xyzzy"));
    }
}
