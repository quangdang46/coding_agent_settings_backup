//! Agent definitions and multi-location resolution.
//!
//! Defines [`AgentConfig`] (the in-memory description of one AI coding
//! agent), the 19 built-in agents from the `asb` reference plus
//! Kiro/Continue/Copilot/Zed/Roo/Trae, and a [`Registry`] that merges
//! built-ins with user-defined entries from [`crate::config::Config`].

use crate::config::Config;
use crate::error::{CasbError, Result};
use crate::util::{expand_tilde, home_dir};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where on the filesystem an agent stores its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocationType {
    /// `~/.something/`.
    HomeDir,
    /// `~/.config/something/` (XDG_CONFIG_HOME fallback).
    XdgConfig,
    /// `~/.local/share/something/` (XDG_DATA_HOME fallback).
    XdgData,
    /// User-defined custom path.
    Custom,
}

/// Whether the location is a directory (recursive copy) or a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocationKind {
    /// A directory. Recursively backed up.
    Directory,
    /// A single file. Backed up verbatim.
    File,
}

/// One source location for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLocation {
    /// Absolute path on the user's machine.
    pub path: PathBuf,
    /// Where this location lives semantically (home, xdg-config, ...).
    pub location_type: LocationType,
    /// File or directory.
    pub kind: LocationKind,
    /// Subdirectory under the backup repo where this location is mirrored.
    pub backup_subdir: String,
}

/// High-level grouping for `casb list` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentCategory {
    /// CLI coding assistants.
    CliCoding,
    /// IDE-integrated agents.
    Ide,
    /// Misc / general assistants.
    Assistant,
}

/// Full definition of an AI coding agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Stable identifier used in CLI commands.
    pub key: String,
    /// Human-readable name.
    pub display_name: String,
    /// Where this agent stores files. May contain multiple locations.
    pub locations: Vec<AgentLocation>,
    /// Category for grouping in lists.
    pub category: AgentCategory,
}

impl AgentConfig {
    /// Determine whether at least one of the agent's locations exists on disk.
    pub fn is_installed(&self) -> bool {
        self.locations.iter().any(|loc| loc.path.exists())
    }

    /// Iterator of locations that actually exist on disk.
    pub fn installed_locations(&self) -> impl Iterator<Item = &AgentLocation> {
        self.locations.iter().filter(|l| l.path.exists())
    }
}

/// Convenience constructor for [`AgentLocation`] in built-ins.
fn loc_dir(path: PathBuf, ty: LocationType, sub: &str) -> AgentLocation {
    AgentLocation {
        path,
        location_type: ty,
        kind: LocationKind::Directory,
        backup_subdir: sub.to_string(),
    }
}

fn loc_file(path: PathBuf, ty: LocationType, sub: &str) -> AgentLocation {
    AgentLocation {
        path,
        location_type: ty,
        kind: LocationKind::File,
        backup_subdir: sub.to_string(),
    }
}

/// Build the list of 19 built-in agents.
///
/// Multi-location agents (claude, opencode) carry several entries, each with
/// a distinct `backup_subdir` so that a flat-merge into a single backup repo
/// avoids collisions.
pub fn builtin_agents() -> Result<Vec<AgentConfig>> {
    let home = home_dir()?;
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let xdg_data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"));

    let agents = vec![
        // 1. Claude Code (multi-location)
        AgentConfig {
            key: "claude".into(),
            display_name: "Claude Code".into(),
            category: AgentCategory::CliCoding,
            locations: vec![
                loc_dir(home.join(".claude"), LocationType::HomeDir, ".claude/home"),
                loc_dir(xdg_data.join("claude"), LocationType::XdgData, ".claude/data"),
                loc_file(home.join(".claude.json"), LocationType::HomeDir, ".claude/root"),
            ],
        },
        // 2. Codex
        AgentConfig {
            key: "codex".into(),
            display_name: "OpenAI Codex CLI".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".codex"), LocationType::HomeDir, ".codex")],
        },
        // 3. Cursor
        AgentConfig {
            key: "cursor".into(),
            display_name: "Cursor".into(),
            category: AgentCategory::Ide,
            locations: vec![loc_dir(home.join(".cursor"), LocationType::HomeDir, ".cursor")],
        },
        // 4. Gemini
        AgentConfig {
            key: "gemini".into(),
            display_name: "Google Gemini".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".gemini"), LocationType::HomeDir, ".gemini")],
        },
        // 5. Cline
        AgentConfig {
            key: "cline".into(),
            display_name: "Cline".into(),
            category: AgentCategory::Ide,
            locations: vec![loc_dir(home.join(".cline"), LocationType::HomeDir, ".cline")],
        },
        // 6. Amp
        AgentConfig {
            key: "amp".into(),
            display_name: "Amp (Sourcegraph)".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".amp"), LocationType::HomeDir, ".amp")],
        },
        // 7. Aider
        AgentConfig {
            key: "aider".into(),
            display_name: "Aider".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".aider"), LocationType::HomeDir, ".aider")],
        },
        // 8. OpenCode (multi-location)
        AgentConfig {
            key: "opencode".into(),
            display_name: "OpenCode".into(),
            category: AgentCategory::CliCoding,
            locations: vec![
                loc_dir(
                    xdg_config.join("opencode"),
                    LocationType::XdgConfig,
                    ".opencode/config",
                ),
                loc_dir(xdg_data.join("opencode"), LocationType::XdgData, ".opencode/data"),
            ],
        },
        // 9. Factory
        AgentConfig {
            key: "factory".into(),
            display_name: "Factory Droid".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".factory"), LocationType::HomeDir, ".factory")],
        },
        // 10. Windsurf
        AgentConfig {
            key: "windsurf".into(),
            display_name: "Windsurf".into(),
            category: AgentCategory::Ide,
            locations: vec![loc_dir(home.join(".windsurf"), LocationType::HomeDir, ".windsurf")],
        },
        // 11. Plandex
        AgentConfig {
            key: "plandex".into(),
            display_name: "Plandex".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(
                home.join(".plandex-home"),
                LocationType::HomeDir,
                ".plandex",
            )],
        },
        // 12. Qwen Code
        AgentConfig {
            key: "qwencode".into(),
            display_name: "Qwen Code".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".qwen"), LocationType::HomeDir, ".qwen")],
        },
        // 13. Amazon Q
        AgentConfig {
            key: "amazonq".into(),
            display_name: "Amazon Q".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".q"), LocationType::HomeDir, ".amazonq")],
        },
        // 14. Kiro (NEW)
        AgentConfig {
            key: "kiro".into(),
            display_name: "Kiro".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".kiro"), LocationType::HomeDir, ".kiro")],
        },
        // 15. Continue (NEW)
        AgentConfig {
            key: "continue".into(),
            display_name: "Continue".into(),
            category: AgentCategory::Ide,
            locations: vec![loc_dir(home.join(".continue"), LocationType::HomeDir, ".continue")],
        },
        // 16. GitHub Copilot CLI (NEW)
        AgentConfig {
            key: "copilot".into(),
            display_name: "GitHub Copilot CLI".into(),
            category: AgentCategory::CliCoding,
            locations: vec![loc_dir(home.join(".copilot"), LocationType::HomeDir, ".copilot")],
        },
        // 17. Zed (NEW)
        AgentConfig {
            key: "zed".into(),
            display_name: "Zed Editor".into(),
            category: AgentCategory::Ide,
            locations: vec![loc_dir(home.join(".zed"), LocationType::HomeDir, ".zed")],
        },
        // 18. Roo (NEW)
        AgentConfig {
            key: "roo".into(),
            display_name: "Roo Code".into(),
            category: AgentCategory::Ide,
            locations: vec![loc_dir(home.join(".roo"), LocationType::HomeDir, ".roo")],
        },
        // 19. Trae (NEW)
        AgentConfig {
            key: "trae".into(),
            display_name: "Trae".into(),
            category: AgentCategory::Ide,
            locations: vec![loc_dir(home.join(".trae"), LocationType::HomeDir, ".trae")],
        },
    ];

    Ok(agents)
}

/// In-memory registry of all known agents (built-in + user-configured).
#[derive(Debug, Clone)]
pub struct Registry {
    agents: Vec<AgentConfig>,
}

impl Registry {
    /// Build a registry by merging built-in agents with overrides from `cfg`.
    ///
    /// User entries with `enabled = false` remove the matching built-in.
    /// User entries with explicit `locations` replace the built-in's
    /// locations entirely. Entirely new keys are appended.
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let mut agents = builtin_agents()?;

        for (key, custom) in &cfg.agents {
            if !custom.enabled {
                agents.retain(|a| a.key != *key);
                continue;
            }
            // Compute resolved locations (custom or inherit existing).
            let resolved_locations: Vec<AgentLocation> = if custom.locations.is_empty() {
                Vec::new()
            } else {
                custom
                    .locations
                    .iter()
                    .map(|p| {
                        let path = expand_tilde(p);
                        let kind = if path.is_file() {
                            LocationKind::File
                        } else {
                            LocationKind::Directory
                        };
                        AgentLocation {
                            path,
                            location_type: LocationType::Custom,
                            kind,
                            backup_subdir: format!(".{key}"),
                        }
                    })
                    .collect()
            };

            if let Some(existing) = agents.iter_mut().find(|a| a.key == *key) {
                if let Some(name) = &custom.display_name {
                    existing.display_name = name.clone();
                }
                if !resolved_locations.is_empty() {
                    existing.locations = resolved_locations;
                }
            } else {
                agents.push(AgentConfig {
                    key: key.clone(),
                    display_name: custom.display_name.clone().unwrap_or_else(|| key.clone()),
                    category: AgentCategory::CliCoding,
                    locations: resolved_locations,
                });
            }
        }

        agents.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(Self { agents })
    }

    /// Get every agent in the registry.
    pub fn all(&self) -> &[AgentConfig] {
        &self.agents
    }

    /// Look up an agent by key.
    pub fn get(&self, key: &str) -> Result<&AgentConfig> {
        self.agents
            .iter()
            .find(|a| a.key == key)
            .ok_or_else(|| CasbError::AgentNotFound { key: key.into() })
    }

    /// Iterate over only the agents that are installed (have at least one
    /// existing location on disk).
    pub fn installed(&self) -> impl Iterator<Item = &AgentConfig> {
        self.agents.iter().filter(|a| a.is_installed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nineteen_builtin_agents_with_unique_keys() {
        let agents = builtin_agents().unwrap();
        assert_eq!(agents.len(), 19);
        let mut keys: Vec<_> = agents.iter().map(|a| a.key.as_str()).collect();
        keys.sort();
        let mut deduped = keys.clone();
        deduped.dedup();
        assert_eq!(keys.len(), deduped.len(), "agent keys must be unique");
    }

    #[test]
    fn claude_has_three_locations() {
        let agents = builtin_agents().unwrap();
        let claude = agents.iter().find(|a| a.key == "claude").unwrap();
        assert_eq!(claude.locations.len(), 3);
        assert!(claude
            .locations
            .iter()
            .any(|l| l.kind == LocationKind::File));
    }

    #[test]
    fn opencode_has_two_locations() {
        let agents = builtin_agents().unwrap();
        let oc = agents.iter().find(|a| a.key == "opencode").unwrap();
        assert_eq!(oc.locations.len(), 2);
    }

    #[test]
    fn registry_merges_custom_agent() {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "myagent".to_string(),
            crate::config::CustomAgentConfig {
                enabled: true,
                display_name: Some("My Agent".into()),
                locations: vec!["/tmp/myagent".into()],
                exclusions: vec![],
            },
        );
        let reg = Registry::from_config(&cfg).unwrap();
        assert!(reg.get("myagent").is_ok());
        assert_eq!(reg.get("myagent").unwrap().display_name, "My Agent");
    }

    #[test]
    fn registry_disables_builtin() {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "claude".to_string(),
            crate::config::CustomAgentConfig {
                enabled: false,
                ..Default::default()
            },
        );
        let reg = Registry::from_config(&cfg).unwrap();
        assert!(reg.get("claude").is_err());
    }
}
