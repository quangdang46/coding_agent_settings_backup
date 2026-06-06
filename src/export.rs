//! `casb export` and `casb import` implementations.
//!
//! Export writes a per-agent backup repo as a `tar.gz` archive (file path or
//! `-` for stdout). Import reads the archive (file path or `-` for stdin)
//! and unpacks it under the configured backup root.

use crate::agent::Registry;
use crate::backup::agent_repo_path;
use crate::config::Config;
use crate::error::{CasbError, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

/// Export an agent's backup repo.
///
/// `dest` is `None` for the default file path (`<key>-<timestamp>.tar.gz`
/// in the current directory), `Some("-")` for stdout, or `Some(path)` for
/// an explicit file.
pub fn export_agent(
    cfg: &Config,
    registry: &Registry,
    key: &str,
    dest: Option<&str>,
) -> Result<Option<std::path::PathBuf>> {
    let agent = registry.get(key)?;
    let repo_path = agent_repo_path(&cfg.backup_root(), agent);
    if !repo_path.exists() {
        return Err(CasbError::NoBackupRepo {
            key: agent.key.clone(),
        });
    }

    let stdout_mode = matches!(dest, Some("-"));
    let resolved_path: Option<std::path::PathBuf> = if stdout_mode {
        None
    } else {
        let pb = match dest {
            Some(p) => crate::util::expand_tilde(p),
            None => {
                let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                std::path::PathBuf::from(format!("{}-{}.tar.gz", agent.key, stamp))
            }
        };
        Some(pb)
    };

    if stdout_mode {
        let stdout = io::stdout();
        let lock = stdout.lock();
        let enc = GzEncoder::new(lock, Compression::default());
        write_tar(enc, &repo_path, &agent.key)?;
    } else if let Some(path) = &resolved_path {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = File::create(path)?;
        let enc = GzEncoder::new(file, Compression::default());
        write_tar(enc, &repo_path, &agent.key)?;
    }
    Ok(resolved_path)
}

fn write_tar<W: Write>(enc: GzEncoder<W>, repo_path: &Path, key: &str) -> Result<()> {
    let agent_dir = format!(".{key}");
    // If the agent directory exists on disk, use it directly.
    // Otherwise, restore from git first (the backup commit removed the
    // working tree to save disk space).
    let need_cleanup = if repo_path.join(&agent_dir).exists() {
        false
    } else {
        // Restore agent directory from git HEAD.
        std::process::Command::new("git")
            .current_dir(repo_path)
            .args(["checkout", "HEAD", "--", &agent_dir])
            .output()?;
        true
    };
    let mut tar = tar::Builder::new(enc);
    if repo_path.join(&agent_dir).exists() {
        tar.append_dir_all(&agent_dir, repo_path.join(&agent_dir))?;
    }
    if repo_path.join(".git").exists() {
        tar.append_dir_all(".git", repo_path.join(".git"))?;
    }
    let enc = tar.into_inner()?;
    enc.finish()?;
    // Clean up restored directory if we created it.
    if need_cleanup {
        let _ = std::fs::remove_dir_all(repo_path.join(&agent_dir));
    }
    Ok(())
}

/// Import a backup archive. `source` is `None` for stdin (also accepts
/// `Some("-")`), or `Some(path)` for a file.
///
/// Returns the path to the agent's restored backup repo.
pub fn import_archive(cfg: &Config, source: Option<&str>) -> Result<std::path::PathBuf> {
    let backup_root = cfg.backup_root();
    std::fs::create_dir_all(&backup_root)?;
    let stdin_mode = matches!(source, None | Some("-"));

    if stdin_mode {
        let stdin = io::stdin();
        let lock = stdin.lock();
        let dec = GzDecoder::new(lock);
        unpack(dec, &backup_root)?;
    } else if let Some(path) = source {
        let p = crate::util::expand_tilde(path);
        let file = File::open(&p)?;
        let dec = GzDecoder::new(file);
        unpack(dec, &backup_root)?;
    }
    Ok(backup_root)
}

fn unpack<R: Read>(dec: GzDecoder<R>, dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(dec);
    tar.set_overwrite(true);
    tar.unpack(dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentCategory, AgentConfig, AgentLocation, LocationKind, LocationType};
    use crate::backup::backup_agents;
    use tempfile::tempdir;

    fn cfg_for(root: &std::path::Path) -> Config {
        let mut c = Config::default();
        c.general.backup_root = root.display().to_string();
        c.backup.use_rsync = false;
        c
    }

    fn registry_with(agent: &AgentConfig) -> Registry {
        let mut cfg = Config::default();
        cfg.agents.insert(
            agent.key.clone(),
            crate::config::CustomAgentConfig {
                enabled: true,
                display_name: Some(agent.display_name.clone()),
                locations: agent
                    .locations
                    .iter()
                    .map(|l| l.path.display().to_string())
                    .collect(),
                exclusions: vec![],
            },
        );
        Registry::from_config(&cfg).unwrap()
    }

    #[test]
    fn export_to_file_then_import_round_trips() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "data").unwrap();
        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "e1".into(),
            display_name: "e1".into(),
            category: AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".e1".into(),
            }],
        };
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["e1".into()], None, false, false).unwrap();
        let archive_dir = tempdir().unwrap();
        let archive = archive_dir.path().join("e1.tar.gz");
        let path = export_agent(&cfg, &reg, "e1", Some(archive.to_str().unwrap())).unwrap();
        assert_eq!(path.as_deref(), Some(archive.as_path()));
        assert!(archive.exists());

        // Import into a fresh root.
        let new_root = tempdir().unwrap();
        let mut new_cfg = cfg_for(new_root.path());
        new_cfg.general.backup_root = new_root.path().display().to_string();
        let dest = import_archive(&new_cfg, Some(archive.to_str().unwrap())).unwrap();
        assert!(dest.join(".e1").exists());
        assert!(dest.join(".e1/a.txt").exists());
    }

    #[test]
    fn export_unknown_agent_errors() {
        let backup_root = tempdir().unwrap();
        let cfg = cfg_for(backup_root.path());
        let reg = Registry::from_config(&Config::default()).unwrap();
        let res = export_agent(&cfg, &reg, "no-such-agent", None);
        assert!(res.is_err());
    }
}
