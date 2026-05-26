# `casb` — Coding Agent Settings Backup

[![Rust](https://img.shields.io/badge/rust-1.79%2B-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

`casb` (Coding Agent Settings Backup) is a Rust-powered CLI that backs up,
versions, and restores configuration folders for AI coding agents
(Claude Code, Codex, Cursor, Gemini, OpenCode, Kiro, …) using a **single
shared git repository** at the backup root. It is a feature-complete,
type-safe port of the original [`asb`](https://github.com/quangdang46/agent_settings_backup_script)
bash script with first-class support for multi-location agents and SQLite
state files.

## Features

- **19 built-in agents**: claude, codex, cursor, gemini, cline, amp, aider,
  opencode, factory, windsurf, plandex, qwencode, amazonq, kiro, continue,
  copilot, zed, roo, trae.
- **Multi-location support**: Claude (`~/.claude/`, `~/.local/share/claude/`,
  `~/.claude.json`) and OpenCode (`~/.config/opencode/`,
  `~/.local/share/opencode/`) are merged into a single backup repo per agent.
- **Single shared git repository** at `~/.agent_settings_backups/.git`.
- **Smart filtering** with sensible defaults plus per-agent `.casbignore`.
- **SQLite state databases are backed up** (only `*.sqlite3-wal` /
  `*.sqlite3-shm` temp files are excluded).
- **Multiple output formats**: text (default), `--json`, `--format toon`.
- **Automation**: schedule via `systemd` user timers or `cron`; pre/post
  backup/restore hooks.
- **Portability**: `export` / `import` to `tar.gz` (with `-` for stdin/stdout).
- **Discovery**: `casb discover` scans `$HOME` for new dot-directories that
  look like AI agent config dirs.
- **Doctor**: `casb doctor` runs a battery of health checks.
- **Shell completion**: bash, zsh, fish.
- **No `git2` / no `libgit2` / no `nix`**: uses `std::process::Command` —
  smaller binary (~2.9 MB), faster compile, cross-platform.

## Install

### Linux / macOS (curl | bash)

```sh
curl -fsSL "https://raw.githubusercontent.com/quangdang46/coding_agent_settings_backup/main/install.sh?$(date +%s)" | bash
```

The cache-busting query string forces `curl` to fetch the latest revision.

Environment knobs:

| Variable      | Default                                                              | Notes                                           |
| ------------- | -------------------------------------------------------------------- | ----------------------------------------------- |
| `CASB_REPO`   | `https://github.com/quangdang46/coding_agent_settings_backup`        | Source repository.                              |
| `CASB_REF`    | `main`                                                               | Git ref to install from.                        |
| `CASB_PREFIX` | `$HOME/.cargo`                                                       | Install root (binary lands at `$PREFIX/bin/casb`). |

### Windows (PowerShell)

```powershell
irm "https://raw.githubusercontent.com/quangdang46/coding_agent_settings_backup/main/install.ps1?$(Get-Date -UFormat %s)" | iex
```

Both installers require a Rust toolchain (`cargo`, `rustc`) and `git` on
`PATH`. They run `cargo install --git ... --locked --force` under the hood.

### From source

```sh
git clone https://github.com/quangdang46/coding_agent_settings_backup
cd coding_agent_settings_backup
cargo install --path . --locked
```

## Quick start

```sh
casb init                 # create the backup root (~/.agent_settings_backups)
casb list                 # show every agent and which are installed
casb backup               # back up every installed agent
casb backup claude codex  # back up specific agents
casb history claude       # show backup history for one agent
casb diff claude          # show changes vs the latest backup
casb restore claude       # preview + confirm restore from HEAD
casb tag create claude v1 # tag the current backup
casb stats                # repo size, commit count, source size
casb verify               # git fsck across every backup repo
casb doctor               # health check
```

All commands accept these global flags:

| Flag           | Purpose                                              |
| -------------- | ---------------------------------------------------- |
| `--dry-run`    | Show what would happen without making changes.      |
| `--force`      | Skip confirmation prompts.                           |
| `--verbose`    | Show detailed output (also enables `tracing` debug). |
| `--quiet`      | Suppress non-error output.                           |
| `--json`       | Machine-readable JSON envelope.                      |
| `--format`     | `text` (default), `json`, or `toon`.                 |
| `--config <p>` | Override the config file path.                       |

## Configuration

Default location: `~/.config/casb/config.toml` (override with `CASB_CONFIG`).

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
use_rsync = true        # falls back to a pure-Rust walker if rsync absent
checksum_verify = false

[schedule]
method = "systemd"      # systemd | cron | none
interval = "daily"      # hourly | daily | weekly

# Optional custom / overridden agents
[agents.myagent]
enabled = true
display_name = "My Agent"
locations = ["~/.myagent/"]
exclusions = []
```

Environment overrides:

| Variable             | Maps to                  |
| -------------------- | ------------------------ |
| `CASB_BACKUP_ROOT`   | `general.backup_root`    |
| `CASB_AUTO_COMMIT`   | `general.auto_commit`    |
| `CASB_VERBOSE`       | `general.verbose`        |
| `CASB_OUTPUT_FORMAT` | `general.output_format`  |
| `CASB_CONFIG`        | Config file path itself. |

## Hooks

Place executable scripts in:

- `~/.config/casb/pre-backup.d/`
- `~/.config/casb/post-backup.d/`
- `~/.config/casb/pre-restore.d/`
- `~/.config/casb/post-restore.d/`

Each script is invoked with the agent key as its first argument and
`CASB_HOOK` / `CASB_AGENT` environment variables. Hooks run in alphabetical
order; a non-zero exit aborts the operation.

## Backup repository layout

```
~/.agent_settings_backups/
├── .git/                   # ONE shared git repository for all agents
├── README.md
├── .claude/                # from ~/.claude/
│   ├── home/               # settings, hooks, skills (projects/, transcripts/, plans/ excluded)
│   ├── data/               # from ~/.local/share/claude/
│   └── root/              # ~/.claude.json
├── .codex/                 # from ~/.codex/
│   └── …
├── .opencode/              # merged from 2 locations
│   ├── config/             # from ~/.config/opencode/
│   └── data/               # from ~/.local/share/opencode/
└── …
```

## Build / develop

```sh
cargo build --release       # produces ./target/release/casb (~2.9 MB)
cargo test                  # 65 unit + 12 E2E tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

The mock-agent generator under `scripts/generate_mock_agents.sh` is used by
the E2E suite to produce realistic agent layouts in an isolated tempdir.

## License

MIT — see [LICENSE](LICENSE).
