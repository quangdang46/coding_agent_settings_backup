//! `casb discover` implementation.
//!
//! Scans the user's home directory for dot-directories that look like AI
//! agent config dirs but are not in the built-in agent list.

use crate::agent::Registry;
use crate::error::Result;
use crate::util::home_dir;
use std::collections::HashSet;
use std::path::PathBuf;

/// Discovery result entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Discovered {
    /// Suggested agent key (lowercase, dot-stripped).
    pub key: String,
    /// Absolute path of the dot-directory.
    pub path: PathBuf,
    /// Reason this looked like an agent dir.
    pub reason: String,
}

/// Patterns of well-known additional agents not in the built-ins.
const KNOWN_PATTERNS: &[&str] = &[
    "kodu",
    "sourcegraph",
    "tabby",
    "cody",
    "tabnine",
    "supermaven",
    "aide",
    "void",
    "melty",
];

/// Scan the home directory for candidate agent folders.
pub fn discover(registry: &Registry) -> Result<Vec<Discovered>> {
    let home = home_dir()?;
    let known_paths: HashSet<PathBuf> = registry
        .all()
        .iter()
        .flat_map(|a| a.locations.iter().map(|l| l.path.clone()))
        .collect();

    let mut found = Vec::new();
    for entry in std::fs::read_dir(&home)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !name.starts_with('.') || name == ".git" || name == ".cache" {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        if known_paths.contains(&path) {
            continue;
        }
        let stripped = name.trim_start_matches('.').to_string();
        let reason = if KNOWN_PATTERNS
            .iter()
            .any(|p| stripped.eq_ignore_ascii_case(p))
        {
            format!("matches known agent pattern '{stripped}'")
        } else if looks_like_agent_dir(&path) {
            "contains agent-like config files".to_string()
        } else {
            continue;
        };
        found.push(Discovered {
            key: stripped.to_ascii_lowercase(),
            path,
            reason,
        });
    }
    found.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(found)
}

fn looks_like_agent_dir(path: &std::path::Path) -> bool {
    // Heuristic: contains at least one of these tell-tale files.
    let candidates = [
        "config.json",
        "config.toml",
        "settings.json",
        "auth.json",
        "mcp.json",
        "skills",
        "sessions",
        "hooks.json",
    ];
    for c in candidates {
        if path.join(c).exists() {
            return true;
        }
    }
    false
}
