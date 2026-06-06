<div align="center">

# `casb` — Coding Agent Settings Backup

Back up, version, and restore AI coding agent configs — in Rust.

[![License: MIT](https://img.shields.io/github/license/quangdang46/coding_agent_settings_backup?style=for-the-badge)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/quangdang46/coding_agent_settings_backup?style=for-the-badge)](https://github.com/quangdang46/coding_agent_settings_backup/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/quangdang46/coding_agent_settings_backup/ci.yml?style=for-the-badge)](https://github.com/quangdang46/coding_agent_settings_backup/actions)
[![GitHub Stars](https://img.shields.io/github/stars/quangdang46/coding_agent_settings_backup?style=for-the-badge)](https://github.com/quangdang46/coding_agent_settings_backup/stargazers)

</div>

## What is this?

`casb` is a CLI that backs up, snapshots, and restores configuration folders for AI coding agents — Claude Code, Codex, Cursor, Gemini, OpenCode, and 14 more. Each agent gets a **single shared git repository** under `~/.agent_settings_backups/`, giving you full version history, diff, tags, and restore on any agent's settings.

It is a feature-complete, type-safe Rust port of the [`asb`](https://github.com/quangdang46/agent_settings_backup_script) bash script with first-class multi-location agent support, SQLite state backup, parallel operations, and a `doctor` health-check command.

## Quick Start

```sh
# Install via curl | bash (Linux / macOS)
curl -fsSL "https://raw.githubusercontent.com/quangdang46/coding_agent_settings_backup/main/install.sh" | bash

# Or from source
cargo install --git https://github.com/quangdang46/coding_agent_settings_backup --locked

# Initialize backup root
casb init

# Back up all installed agents
casb backup

# See what agents are detected and their backup status
casb list

# Restore a specific agent from the latest backup
casb restore claude
```

See [full CLI reference](#usage) below for all commands.

## Features

- **19 built-in agents**: claude, codex, cursor, gemini, cline, amp, aider, opencode, factory, windsurf, plandex, qwencode, amazonq, kiro, continue, copilot, zed, roo, trae.
- **Multi-location support**: Agents with multiple config dirs (Claude: 3 locations, OpenCode: 2) are merged into a single backup repo.
- **Single shared git repository** at `~/.agent_settings_backups/.git` — no per-agent `.git` sprawl.
- **Smart filtering**: Sensible defaults plus per-agent `.casbignore`.
- **SQLite state databases are backed up** — only `*.sqlite3-wal` / `*.sqlite3-shm` temp files excluded.
- **Multiple output formats**: `text` (default), `--json`, `--format toon`.
- **Parallel backup**: `casb backup --parallel` runs agent backups concurrently.
- **Automation**: Schedule via `systemd` user timers or `cron`; pre/post backup/restore hooks.
- **Export / Import**: `tar.gz` archives with stdin/stdout pipe support.
- **Discovery**: `casb discover` scans `$HOME` for new dot-directories that look like AI agent configs.
- **Doctor**: `casb doctor` runs a battery of health checks (git, rsync, disk space, config validity, per-repo `git fsck`).
- **Shell completion**: bash, zsh, fish.
- **No `git2` / no `libgit2` / no `nix`**: uses `std::process::Command` — ~2.9 MB binary, fast compile, cross-platform.

## Usage

```
casb [OPTIONS] <COMMAND>
```

**Global flags:**

| Flag | Purpose |
|------|---------|
| `-n`, `--dry-run` | Show what would happen without making changes |
| `-f`, `--force` | Skip confirmation prompts |
| `-v`, `--verbose` | Detailed output (enables `tracing` debug) |
| `-q`, `--quiet` | Suppress non-error output |
| `--json` | Machine-readable JSON envelope |
| `--format <FMT>` | `text` (default), `json`, or `toon` |
| `--config <PATH>` | Override config file path |

**Commands:**

| Command | Description |
|---------|-------------|
| `init` | Initialize the backup root directory |
| `backup [AGENTS...]` | Back up one or more agents (all if none specified) |
| `restore <AGENT> [REF]` | Restore from a backup commit or tag |
| `export <AGENT> [FILE]` | Export an agent backup as `tar.gz` (`-` for stdout) |
| `import [FILE]` | Import from a `tar.gz` archive (`-` for stdin) |
| `list` | Show every agent and which are installed |
| `history <AGENT>` | Show backup history for an agent |
| `diff <AGENT>` | Show changes since the latest backup |
| `tag <create\|list\|delete> <AGENT> <NAME>` | Manage backup tags |
| `verify [AGENTS...]` | Run `git fsck` across backup repos |
| `stats [AGENT]` | Repo size, commit count, source size |
| `discover` | Scan for newly installed AI agents |
| `schedule` | Manage automated backup schedules |
| `hooks` | List configured hook scripts |
| `config` | Get/set configuration values |
| `doctor` | Run health checks against the casb installation |
| `completion <SHELL>` | Generate shell completion (bash, zsh, fish) |

## Configuration

Default location: `~/.config/casb/config.toml` (override with `CASB_CONFIG` or `--config`).

```toml
[general]
backup_root = "~/.agent_settings_backups"
auto_commit = true
verbose = false
quiet = false
output_format = "text"

[backup]
exclusions = [
    "*.log", "*.tmp", "*.temp", "*.swp", "*~",
    ".DS_Store", "Thumbs.db",
    "**/cache/**", "**/Cache/**", "**/.cache/**",
    "*.sqlite3-wal", "*.sqlite3-shm",
    "**/paste-cache/**",
]
use_rsync = true         # falls back to pure-Rust walker if rsync absent
checksum_verify = false

[schedule]
method = "systemd"       # systemd | cron | none
interval = "daily"       # hourly | daily | weekly

# Custom agent overrides
[agents.myagent]
enabled = true
display_name = "My Agent"
locations = ["~/.myagent/"]
exclusions = []
```

**Environment overrides:**

| Variable | Maps to |
|----------|---------|
| `CASB_BACKUP_ROOT` | `general.backup_root` |
| `CASB_AUTO_COMMIT` | `general.auto_commit` |
| `CASB_VERBOSE` | `general.verbose` |
| `CASB_OUTPUT_FORMAT` | `general.output_format` |
| `CASB_CONFIG` | Config file path itself |

## Project Structure

```
scripts/
├── check_features.sh         # Validate featured-matrix completeness
└── generate_mock_agents.sh   # Create mock agent layouts for E2E tests
src/
├── main.rs                   # Entry point, tracing setup
├── lib.rs                    # Public API re-exports
├── cli.rs                    # clap derive CLI definitions
├── commands.rs               # Command dispatch logic
├── config.rs                 # TOML config loading + env var overrides
├── agent.rs                  # Agent definitions, discovery
├── backup.rs                 # Backup orchestration (sync + git + commit)
├── restore.rs                # Restore with preview + confirm
├── export.rs                 # tar.gz export / import
├── history.rs                # Git log / tag history
├── diff.rs                   # Current-vs-backup comparison
├── verify.rs                 # git fsck integrity checks
├── stats.rs                  # Backup statistics
├── schedule.rs               # cron / systemd scheduling
├── hooks.rs                  # Pre/post hook execution
├── completion.rs             # Shell completion generators
├── output.rs                 # text / JSON / TOON formatters
├── filter.rs                 # Exclusion / ignore rules
├── doctor.rs                 # Health check diagnostics
├── tag.rs                    # Git tag management
├── sync.rs                   # rsync / cp file sync
├── discover.rs               # Agent detection scanner
├── util.rs                   # Shared helpers
├── error.rs                  # Structured error types
└── toon.rs                   # TOON format rendering
tests/
└── e2e.rs                    # End-to-end integration tests
```

## Documentation

| Resource | Description |
|----------|-------------|
| [Plan & Architecture](PLAN.md) | Full architecture document with agent definitions, data flow, and phase breakdown |
| [Install Script](install.sh) | `curl | bash` installer — Linux / macOS |
| [PowerShell Installer](install.ps1) | `irm | iex` installer — Windows |
| [CI Workflow](.github/workflows/ci.yml) | GitHub Actions — build, test, clippy |
| [Release Workflow](.github/workflows/release.yml) | GitHub Actions — cross-platform release |
| [Feature Check Script](scripts/check_features.sh) | Validate all features against the spec |
| [Mock Agent Generator](scripts/generate_mock_agents.sh) | Generate test fixtures for E2E tests |

## Contributing

PRs and issues are welcome. The project follows standard Rust conventions:

```sh
cargo build --release       # ~2.9 MB binary
cargo test                  # unit + E2E tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

<a href="https://github.com/quangdang46/coding_agent_settings_backup/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=quangdang46/coding_agent_settings_backup" />
</a>

## License

MIT — see [LICENSE](LICENSE).

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=quangdang46/coding_agent_settings_backup&type=Date)](https://star-history.com/#quangdang46/coding_agent_settings_backup&Date)

</div>
