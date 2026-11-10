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
use std::path::{Component, Path};
use tar::EntryType;

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
    // Store symlinks as symlinks rather than following them. Agent
    // directories routinely contain "latest" pointers to files that have
    // since been rotated away; following those dangling links makes tar
    // fail with "No such file or directory" and aborts the whole export.
    tar.follow_symlinks(false);
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
/// The archive is unpacked into a temporary directory *inside* the backup
/// root first. Every entry is checked for path traversal and hostile symlink
/// targets, the extracted `.git` metadata is sanitised (hooks removed,
/// dangerous config keys stripped) and validated, and only then is the tree
/// merged into the backup root. An archive that fails any check leaves the
/// existing backup untouched.
///
/// Returns the path to the agent's restored backup repo.
pub fn import_archive(cfg: &Config, source: Option<&str>) -> Result<std::path::PathBuf> {
    let backup_root = cfg.backup_root();
    std::fs::create_dir_all(&backup_root)?;
    let stdin_mode = matches!(source, None | Some("-"));

    // Unique staging dir on the same filesystem so the final merge is a
    // rename, not a copy.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let staging = backup_root.join(format!(".casb-import-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&staging)?;

    let result = (|| -> Result<()> {
        if stdin_mode {
            let stdin = io::stdin();
            let lock = stdin.lock();
            let dec = GzDecoder::new(lock);
            unpack(dec, &staging)?;
        } else if let Some(path) = source {
            let p = crate::util::expand_tilde(path);
            let file = File::open(&p)?;
            let dec = GzDecoder::new(file);
            unpack(dec, &staging)?;
        }
        sanitize_imported_tree(&staging)?;
        validate_imported_repo(&staging)?;
        merge_into(&staging, &backup_root)
    })();

    // The staging dir is empty once the merge has moved its children out;
    // remove it either way so a failed import leaves no litter behind.
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    Ok(backup_root)
}

/// Unpack a gzipped tar stream into `dest`, one entry at a time so hostile
/// entries can be rejected instead of silently skipped.
fn unpack<R: Read>(dec: GzDecoder<R>, dest: &Path) -> Result<()> {
    let mut ar = tar::Archive::new(dec);
    let entries = ar
        .entries()
        .map_err(|e| CasbError::other(format!("invalid archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| CasbError::other(format!("invalid archive: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| CasbError::other(format!("invalid archive entry path: {e}")))?
            .into_owned();
        // Defence in depth on top of `unpack_in`'s own checks: absolute
        // paths and `..` components are never legitimate in a casb archive.
        if path.is_absolute()
            || path.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(CasbError::InvalidArgument(format!(
                "archive entry escapes the backup root: {}",
                path.display()
            )));
        }
        // Symlinks and hardlinks carry a target that `unpack_in` writes
        // verbatim. An absolute target or one that resolves outside the
        // staging tree would let the archive plant links to arbitrary
        // locations (and later entries could write through them).
        let etype = entry.header().entry_type();
        if matches!(etype, EntryType::Symlink | EntryType::Link) {
            if let Some(target) = entry
                .link_name()
                .map_err(|e| CasbError::other(format!("invalid archive link entry: {e}")))?
            {
                check_link_target(&path, &target)?;
            }
        }
        // `unpack_in` re-validates the destination after canonicalisation,
        // which also catches writes that would traverse a symlink planted
        // by an earlier entry. `false` means the entry was skipped as
        // unsafe — treat that as an error rather than importing a partial
        // tree.
        let unpacked = entry.unpack_in(dest).map_err(|e| {
            CasbError::other(format!(
                "failed to unpack {}: {}",
                path.display(),
                error_chain(&e)
            ))
        })?;
        if !unpacked {
            return Err(CasbError::InvalidArgument(format!(
                "archive entry was rejected as unsafe: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Render an error with its full `source()` chain — tar-rs wraps the real
/// I/O failure one level down, and the top-level message alone is useless.
fn error_chain(e: &io::Error) -> String {
    let mut out = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(cause) = src {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        src = cause.source();
    }
    out
}

/// Ensure a link entry's target stays inside the staging tree.
///
/// `entry_path` is the archive-relative path of the link itself; `target`
/// is resolved relative to the link's parent directory. Absolute targets
/// and targets that climb above the staging root via `..` are rejected.
/// Relative targets that stay inside (including `../sibling` links that
/// resolve within the tree) are allowed — agent directories legitimately
/// contain relative "latest" pointers.
fn check_link_target(entry_path: &Path, target: &Path) -> Result<()> {
    if target.is_absolute() {
        return Err(CasbError::InvalidArgument(format!(
            "archive link {} has an absolute target: {}",
            entry_path.display(),
            target.display()
        )));
    }
    let base = entry_path.parent().unwrap_or_else(|| Path::new(""));
    let mut parts: Vec<std::ffi::OsString> = base
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_os_string()),
            _ => None,
        })
        .collect();
    for comp in target.components() {
        match comp {
            Component::Normal(s) => parts.push(s.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(CasbError::InvalidArgument(format!(
                        "archive link {} escapes the backup root: {}",
                        entry_path.display(),
                        target.display()
                    )));
                }
            }
            // Absolute targets were rejected above; Prefix/RootDir cannot
            // appear in a relative path but reject them defensively.
            _ => {
                return Err(CasbError::InvalidArgument(format!(
                    "archive link {} has an unsafe target: {}",
                    entry_path.display(),
                    target.display()
                )))
            }
        }
    }
    Ok(())
}

/// Neutralise git-metadata attack surface in the freshly unpacked tree.
///
/// Every directory named `.git` is sanitised — the archive layout puts the
/// backup repo at the top level, but a hostile archive could nest additional
/// `.git` directories inside agent data. For each one:
///
/// - `hooks/` is removed entirely. Sample hooks are optional; casb's own
///   repository does not rely on any hook, and an imported executable
///   `pre-commit`/`post-checkout` would otherwise run on the user's next
///   git operation.
/// - `config` is rewritten with dangerous keys stripped: hook/path
///   redirection (`core.hooksPath`, `core.fsmonitor`, `core.sshCommand`,
///   `core.gitProxy`), command-executing drivers (`filter.*`,
///   `diff.* command/textconv/external`), signing programs (`gpg.*.program`),
///   credential helpers, URL rewrites, interactive editors/pagers, and
///   `include`/`includeIf` sections that could pull in further config.
fn sanitize_imported_tree(root: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_dir() || entry.file_name() != ".git" {
            continue;
        }
        let gitdir = entry.path().to_path_buf();
        let hooks = gitdir.join("hooks");
        if hooks.exists() {
            tracing::warn!(
                "import: removing hooks directory from imported git metadata ({})",
                hooks.display()
            );
            std::fs::remove_dir_all(&hooks)?;
        }
        // `info/attributes` can assign filter/diff drivers; the config-side
        // sections are stripped below, but removing the attributes file
        // removes the other half of the pair.
        let attrs = gitdir.join("info").join("attributes");
        if attrs.exists() {
            std::fs::remove_file(&attrs)?;
        }
        sanitize_git_config(&gitdir.join("config"))?;
    }
    Ok(())
}

/// Config keys that must never survive an import, regardless of section.
const DANGEROUS_CONFIG_KEYS: &[&str] = &[
    "hookspath",
    "fsmonitor",
    "sshcommand",
    "gitproxy",
    "pager",
    "editor",
    "excludesfile",
    "attributesfile",
    "worktree",
    "bare",
];

/// Rewrite a git `config` file in place, dropping lines that could execute
/// commands or redirect git behaviour. Missing/unreadable files are left
/// alone — a repo without a config file is still valid.
fn sanitize_git_config(path: &Path) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let mut out = String::with_capacity(text.len());
    let mut section = String::new();
    let mut skip_section = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_lowercase();
            // `include.path` and `includeIf "<condition>"` sections pull in
            // additional config files — an imported repo must not follow them.
            // Drop the section header as well so no empty section remains.
            skip_section = section == "include" || section == "includeif";
            if skip_section {
                continue;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if skip_section {
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            let key = key.trim().to_lowercase();
            let section_dangerous = if section.starts_with("gpg") {
                key == "program"
            } else if section.starts_with("credential") {
                key == "helper"
            } else if section.starts_with("filter") {
                matches!(key.as_str(), "clean" | "smudge" | "process" | "required")
            } else if section.starts_with("diff") {
                matches!(key.as_str(), "command" | "textconv" | "external")
            } else if section.starts_with("url") {
                key == "insteadof"
            } else {
                false
            };
            if section_dangerous || DANGEROUS_CONFIG_KEYS.contains(&key.as_str()) {
                tracing::warn!("import: stripped dangerous git config key `{key}`");
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// Verify the unpacked tree contains a complete, readable git repository.
///
/// An archive whose `.git` is missing `HEAD`, `objects/` or `refs/` — or
/// whose object store git cannot read — would import "successfully" and
/// leave a broken backup root that fails on the next `casb backup`. Reject
/// it here, while the real backup root is still untouched.
fn validate_imported_repo(staging: &Path) -> Result<()> {
    let gitdir = staging.join(".git");
    if !gitdir.is_dir() {
        return Err(CasbError::InvalidArgument(
            "archive does not contain a .git directory; nothing to import".into(),
        ));
    }
    for required in ["HEAD", "objects", "refs"] {
        if !gitdir.join(required).exists() {
            return Err(CasbError::InvalidArgument(format!(
                "archive .git is incomplete: missing `{required}`"
            )));
        }
    }
    let repo = crate::git::Repo::new(staging);
    if !repo.is_valid_repo() {
        return Err(CasbError::InvalidArgument(
            "archive .git is not a readable git repository".into(),
        ));
    }
    let fsck = repo.fsck()?;
    if !fsck.ok {
        return Err(CasbError::InvalidArgument(format!(
            "archive .git failed integrity check: {}",
            fsck.stderr.trim()
        )));
    }
    Ok(())
}

/// Move the contents of `src` into `dst`, merging directory-by-directory.
///
/// Mirrors the old in-place `tar.unpack` overwrite semantics: files present
/// in both trees are replaced by the archive's version, and unrelated
/// existing content is preserved. Everything is renamed (same filesystem),
/// so no data is copied.
fn merge_into(src: &Path, dst: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        merge_entry(&entry.path(), &dst.join(entry.file_name()))?;
    }
    Ok(())
}

fn merge_entry(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    let dst_is_real_dir = dst
        .symlink_metadata()
        .map(|m| m.is_dir() && !m.file_type().is_symlink())
        .unwrap_or(false);
    if meta.is_dir() {
        if dst_is_real_dir {
            merge_into(src, dst)?;
            std::fs::remove_dir(src)?;
            return Ok(());
        }
    } else if dst_is_real_dir {
        // A file in the archive must never silently replace a whole
        // existing directory tree.
        return Err(CasbError::InvalidArgument(format!(
            "archive entry {} conflicts with existing directory {}",
            src.display(),
            dst.display()
        )));
    }
    if dst.symlink_metadata().is_ok() {
        remove_any(dst)?;
    }
    std::fs::rename(src, dst)?;
    Ok(())
}

fn remove_any(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
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

    /// Regression: agent directories contain "latest" pointers to files that
    /// have been rotated away. Following those dangling symlinks made tar
    /// fail with "No such file or directory" and aborted the whole export.
    #[cfg(unix)]
    #[test]
    fn export_survives_broken_symlink() {
        let backup_root = tempdir().unwrap();
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("settings.json"), "{}").unwrap();
        std::os::unix::fs::symlink(
            src.path().join("rotated-away.txt"),
            src.path().join("latest"),
        )
        .unwrap();

        let cfg = cfg_for(backup_root.path());
        let agent = AgentConfig {
            key: "e2".into(),
            display_name: "e2".into(),
            category: crate::agent::AgentCategory::CliCoding,
            locations: vec![AgentLocation {
                path: src.path().to_path_buf(),
                location_type: LocationType::HomeDir,
                kind: LocationKind::Directory,
                backup_subdir: ".e2".into(),
            }],
        };
        let reg = registry_with(&agent);
        backup_agents(&cfg, &reg, &["e2".into()], None, false, false).unwrap();

        let archive_dir = tempdir().unwrap();
        let archive = archive_dir.path().join("e2.tar.gz");
        let path = export_agent(&cfg, &reg, "e2", Some(archive.to_str().unwrap())).unwrap();
        assert!(path.is_some());
        assert!(archive.exists(), "export must produce an archive");
        // The archive is readable and contains the agent's data.
        let out = std::process::Command::new("tar")
            .args(["tzf", archive.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "archive must be valid");
        let listing = String::from_utf8_lossy(&out.stdout);
        assert!(listing.contains(".e2/settings.json"));
    }

    // ----- hostile archive tests (import safety) -----

    fn add_file(b: &mut tar::Builder<Vec<u8>>, name: &str, data: &[u8]) {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, data).unwrap();
    }

    fn add_symlink(b: &mut tar::Builder<Vec<u8>>, name: &str, target: &str) {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_link_name_literal(target.as_bytes()).unwrap();
        h.set_cksum();
        b.append_data(&mut h, name, io::empty()).unwrap();
    }

    /// Serialise `build` output as a .tar.gz and write it to `dir`.
    fn write_archive(
        dir: &Path,
        name: &str,
        build: impl FnOnce(&mut tar::Builder<Vec<u8>>),
    ) -> std::path::PathBuf {
        let mut b = tar::Builder::new(Vec::new());
        build(&mut b);
        let tar_bytes = b.into_inner().unwrap();
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar_bytes).unwrap();
        let gz = enc.finish().unwrap();
        let path = dir.join(name);
        std::fs::write(&path, gz).unwrap();
        path
    }

    /// A minimal but *valid* git repository tree: real `git init` + one
    /// commit, archived as `.git` plus one agent dir.
    fn archive_with_repo(dir: &Path, extra_git_files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let repo_dir = dir.join("repotree");
        let repo = crate::git::Repo::new(&repo_dir);
        repo.init().unwrap();
        std::fs::create_dir_all(repo_dir.join(".e9")).unwrap();
        std::fs::write(repo_dir.join(".e9/settings.json"), "{}").unwrap();
        repo.add_all().unwrap();
        repo.commit("seed").unwrap();
        // A legitimate relative symlink inside the agent dir must survive.
        #[cfg(unix)]
        std::os::unix::fs::symlink("settings.json", repo_dir.join(".e9/latest")).unwrap();
        for (name, data) in extra_git_files {
            let p = repo_dir.join(".git").join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, data).unwrap();
        }
        write_archive(dir, "hostile.tar.gz", |b| {
            // Keep symlinks as symlinks, like the real exporter does.
            b.follow_symlinks(false);
            b.append_dir_all(".git", repo_dir.join(".git")).unwrap();
            b.append_dir_all(".e9", repo_dir.join(".e9")).unwrap();
        })
    }

    #[test]
    fn import_rejects_absolute_symlink_target() {
        let backup_root = tempdir().unwrap();
        let dir = tempdir().unwrap();
        let arc = write_archive(dir.path(), "evil.tar.gz", |b| {
            add_symlink(b, ".e1/link", "/etc/passwd");
        });
        let cfg = cfg_for(backup_root.path());
        let res = import_archive(&cfg, Some(arc.to_str().unwrap()));
        assert!(res.is_err(), "absolute symlink target must be rejected");
        assert!(!backup_root.path().join(".e1").exists());
    }

    #[test]
    fn import_rejects_dotdot_symlink_target() {
        let backup_root = tempdir().unwrap();
        let dir = tempdir().unwrap();
        let arc = write_archive(dir.path(), "evil.tar.gz", |b| {
            add_symlink(b, ".e1/a/link", "../../../etc");
        });
        let cfg = cfg_for(backup_root.path());
        let res = import_archive(&cfg, Some(arc.to_str().unwrap()));
        assert!(res.is_err(), "escaping symlink target must be rejected");
    }

    #[test]
    fn import_allows_relative_symlink_inside_tree() {
        // check_link_target unit-level: a `../sibling` link that stays
        // inside the archive is legitimate.
        check_link_target(Path::new(".e1/a/link"), Path::new("../b/file")).unwrap();
        check_link_target(Path::new(".e1/latest"), Path::new("settings.json")).unwrap();
        assert!(check_link_target(Path::new(".e1/link"), Path::new("../../x")).is_err());
        assert!(check_link_target(Path::new(".e1/link"), Path::new("/abs")).is_err());
    }

    #[test]
    fn import_strips_hooks_and_dangerous_git_config() {
        let backup_root = tempdir().unwrap();
        let dir = tempdir().unwrap();
        let hook = b"#!/bin/sh\ntouch /tmp/casb-pwned\n";
        let poisoned_config = b"\
[core]
\trepositoryformatversion = 0
\tbare = false
\thooksPath = /tmp/evil-hooks
[include]
\tpath = /tmp/evil-include
[includeIf \"gitdir:/tmp/**\"]
\tpath = /tmp/evil-include2
[filter \"evil\"]
\tclean = touch /tmp/pwned-filter
\tsmudge = cat
[diff \"evil\"]
\tcommand = touch /tmp/pwned-diff
\ttextconv = cat
[gpg \"ssh\"]
\tprogram = /tmp/evil-gpg
[credential]
\thelper = !f(){ touch /tmp/pwned-cred;};f
[url \"https://example.com/\"]
\tinsteadOf = gh:
";
        let arc = archive_with_repo(
            dir.path(),
            &[
                ("hooks/pre-commit", hook),
                ("hooks/post-checkout", hook),
                ("config", poisoned_config),
                ("info/attributes", b"* filter=evil diff=evil\n"),
            ],
        );
        let cfg = cfg_for(backup_root.path());
        import_archive(&cfg, Some(arc.to_str().unwrap())).unwrap();

        let gitdir = backup_root.path().join(".git");
        assert!(gitdir.is_dir());
        assert!(
            !gitdir.join("hooks").exists(),
            "imported hooks must be removed"
        );
        assert!(
            !gitdir.join("info/attributes").exists(),
            "imported info/attributes must be removed"
        );
        let config = std::fs::read_to_string(gitdir.join("config")).unwrap();
        for bad in [
            "hooksPath",
            "clean =",
            "smudge",
            "textconv",
            "insteadOf",
            "program =",
            "helper =",
            "/tmp/evil",
        ] {
            assert!(
                !config.contains(bad),
                "config must not contain `{bad}`:\n{config}"
            );
        }
        // The repo must still be valid and usable.
        let repo = crate::git::Repo::new(backup_root.path());
        assert!(repo.is_valid_repo());
        assert!(repo.commit_count().unwrap() >= 1);
        // Agent data and the legit relative symlink survive.
        assert!(backup_root.path().join(".e9/settings.json").exists());
        #[cfg(unix)]
        {
            let link = std::fs::read_link(backup_root.path().join(".e9/latest")).unwrap();
            assert_eq!(link, Path::new("settings.json"));
        }
    }

    #[test]
    fn import_rejects_incomplete_git() {
        let backup_root = tempdir().unwrap();
        let dir = tempdir().unwrap();
        // .git with only HEAD — no objects/, no refs/.
        let arc = write_archive(dir.path(), "broken.tar.gz", |b| {
            add_file(b, ".git/HEAD", b"ref: refs/heads/main\n");
            add_file(b, ".e1/a.txt", b"data");
        });
        let cfg = cfg_for(backup_root.path());
        let res = import_archive(&cfg, Some(arc.to_str().unwrap()));
        assert!(res.is_err(), "incomplete .git must be rejected");
        assert!(
            !backup_root.path().join(".e1").exists(),
            "failed import must leave the backup root untouched"
        );
    }

    #[test]
    fn import_rejects_archive_without_git() {
        let backup_root = tempdir().unwrap();
        let dir = tempdir().unwrap();
        let arc = write_archive(dir.path(), "nogit.tar.gz", |b| {
            add_file(b, ".e1/a.txt", b"data");
        });
        let cfg = cfg_for(backup_root.path());
        let res = import_archive(&cfg, Some(arc.to_str().unwrap()));
        assert!(res.is_err());
    }
}
