//! `casb doctor` implementation.
//!
//! Runs a battery of environment and configuration health checks and
//! returns a structured report. Each check has a `name`, `ok` flag, and
//! optional `detail` string.

use crate::agent::Registry;
use crate::backup::agent_repo_path;
use crate::config::Config;
use crate::error::Result;
use crate::git::Repo;
use crate::hooks::{all_kinds, hooks_dir};
use crate::util::which;
use std::path::Path;

/// Single doctor check.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Check {
    /// Display name.
    pub name: String,
    /// Pass/fail.
    pub ok: bool,
    /// Optional human-readable detail (failure reason or hint).
    pub detail: Option<String>,
}

/// Aggregate doctor report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoctorReport {
    /// Individual checks.
    pub checks: Vec<Check>,
    /// True if every check passed.
    pub all_ok: bool,
}

/// Run the full doctor sequence.
pub fn run(cfg: &Config, registry: &Registry) -> Result<DoctorReport> {
    let mut checks = Vec::new();

    checks.push(check_command_present("git", "required for backup repos"));
    checks.push(check_command_present_optional(
        "rsync",
        "optional, fast sync",
    ));
    checks.push(check_path_writable("backup root", &cfg.backup_root()));

    // Each installed agent — at least one location readable.
    for agent in registry.installed() {
        let mut readable = true;
        let mut details = Vec::new();
        for loc in agent.installed_locations() {
            if std::fs::metadata(&loc.path).is_err() {
                readable = false;
                details.push(format!("{} unreadable", loc.path.display()));
            }
        }
        let detail = if readable {
            None
        } else {
            Some(details.join("; "))
        };
        checks.push(Check {
            name: format!("agent {} source readable", agent.key),
            ok: readable,
            detail,
        });
    }

    // Each existing repo — fsck.
    for agent in registry.all() {
        let repo_path = agent_repo_path(&cfg.backup_root(), agent);
        if !repo_path.join(".git").exists() {
            continue;
        }
        let repo = Repo::new(&repo_path);
        match repo.fsck() {
            Ok(res) => {
                checks.push(Check {
                    name: format!("repo {} fsck", agent.key),
                    ok: res.ok,
                    detail: if res.ok {
                        None
                    } else {
                        Some(res.stderr.lines().take(3).collect::<Vec<_>>().join("; "))
                    },
                });
            }
            Err(e) => checks.push(Check {
                name: format!("repo {} fsck", agent.key),
                ok: false,
                detail: Some(e.to_string()),
            }),
        }
    }

    // Hooks dirs exist (informational only — pass if dir absent).
    for kind in all_kinds() {
        let dir = hooks_dir(kind)?;
        let ok = !dir.exists() || dir.is_dir();
        let detail = if dir.exists() {
            Some(format!("found at {}", dir.display()))
        } else {
            None
        };
        checks.push(Check {
            name: format!("hooks dir {}", kind.dir_name()),
            ok,
            detail,
        });
    }

    // Config file readable (or absent → defaults used).
    let cfg_path = Config::config_path()?;
    if cfg_path.exists() {
        match std::fs::read_to_string(&cfg_path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(_) => checks.push(Check {
                    name: "config file".into(),
                    ok: true,
                    detail: Some(cfg_path.display().to_string()),
                }),
                Err(e) => checks.push(Check {
                    name: "config file".into(),
                    ok: false,
                    detail: Some(format!("parse error: {e}")),
                }),
            },
            Err(e) => checks.push(Check {
                name: "config file".into(),
                ok: false,
                detail: Some(e.to_string()),
            }),
        }
    } else {
        checks.push(Check {
            name: "config file".into(),
            ok: true,
            detail: Some(format!(
                "absent (defaults will be used) [{}]",
                cfg_path.display()
            )),
        });
    }

    // Schedule status — informational, never fails.
    if let Ok(status) = crate::schedule::status() {
        checks.push(Check {
            name: "schedule".into(),
            ok: true,
            detail: if status.installed {
                Some(format!(
                    "{:?}: {}",
                    status.method.unwrap_or(crate::schedule::Method::Cron),
                    status.trigger.unwrap_or_default()
                ))
            } else {
                Some("not installed".into())
            },
        });
    }

    // Disk space available at the backup root.
    if let Some(detail) = disk_space_detail(&cfg.backup_root()) {
        checks.push(Check {
            name: "disk space".into(),
            ok: true,
            detail: Some(detail),
        });
    }

    let all_ok = checks.iter().all(|c| c.ok);
    Ok(DoctorReport { checks, all_ok })
}

fn check_command_present(name: &str, _hint: &str) -> Check {
    let ok = which(name);
    Check {
        name: format!("{name} on PATH"),
        ok,
        detail: if ok { None } else { Some("not found".into()) },
    }
}

fn check_command_present_optional(name: &str, hint: &str) -> Check {
    let ok = which(name);
    Check {
        name: format!("{name} on PATH"),
        ok: true, // optional commands never fail the doctor.
        detail: if ok {
            Some("present".into())
        } else {
            Some(format!("not found ({hint})"))
        },
    }
}

fn check_path_writable(label: &str, path: &Path) -> Check {
    if !path.exists() {
        return Check {
            name: format!("{label} ({})", path.display()),
            ok: true, // will be created on demand
            detail: Some("does not exist (will be created)".into()),
        };
    }
    let probe = path.join(".casb-write-probe");
    let writable = std::fs::write(&probe, b"x").is_ok();
    if writable {
        let _ = std::fs::remove_file(&probe);
    }
    Check {
        name: format!("{label} writable"),
        ok: writable,
        detail: if writable {
            Some(path.display().to_string())
        } else {
            Some(format!("cannot write under {}", path.display()))
        },
    }
}

fn disk_space_detail(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    if cfg!(windows) {
        disk_space_windows(path)
    } else {
        disk_space_unix(path)
    }
}

fn disk_space_unix(path: &Path) -> Option<String> {
    let out = std::process::Command::new("df")
        .arg("-h")
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut iter = text.lines();
    iter.next()?; // header
    let line = iter.next()?;
    Some(line.trim().to_string())
}

fn disk_space_windows(path: &Path) -> Option<String> {
    let drive = path.to_str()?.chars().next()?.to_ascii_uppercase();
    let script = format!(
        "$d = Get-PSDrive -Name '{}' -ErrorAction SilentlyContinue; \
         if ($d) {{ '{{}}: {{0:N1}} GB free / {{1:N1}} GB total' -f $d.Name, \
         ($d.Free / 1GB), (($d.Used + $d.Free) / 1GB) }}",
        drive
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}
