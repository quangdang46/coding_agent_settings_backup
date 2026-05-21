# PLAN: `coding_agent_settings_backup` — Rust Port of `asb`

## 1. RESEARCH FINDINGS

### What `asb` (v0.3.0) Currently Does

The original bash script is a **4,083-line** single-file CLI that:

| Feature | Status | Notes |
|---------|--------|-------|
| **Agent definitions** | 13 built-in agents | `claude`, `codex`, `cursor`, `gemini`, `cline`, `amp`, `aider`, `opencode`, `factory`, `windsurf`, `plandex`, `qwencode`, `amazonq` |
| **Discovery** | 13 discovery patterns | `kodu`, `continue`, `sourcegraph`, `tabby`, `cody`, `copilot`, `tabnine`, `supermaven`, `aide`, `roo`, `void`, `zed`, `trae`, `melty` |
| **Custom agents** | `~/.config/asb/custom_agents` | `key=folder` format |
| **Backup** | `rsync` → `cp` fallback | Excludes `.git`, `.gitignore`, `*.log`, `cache/`, `Cache/`, `.cache/`, `*.sqlite3-wal`, `*.sqlite3-shm` |
| **Restore** | Preview + confirm + rsync | Extracts backup to temp, `diff -rq` preview, y/N confirmation |
| **Export/Import** | `tar.gz` archives | Pipe support (`-` for stdin/stdout) |
| **Git versioning** | Per-agent repos | Each agent = separate git repo under `~/.agent_settings_backups/` |
| **Tags** | Git tags for named backups | Create, list, delete, restore-from-tag |
| **History** | `git log` formatted output | JSON + human-readable |
| **Diff** | `diff -rq` current vs backup | Added/removed/modified lists |
| **Verify** | `git fsck` + HEAD check | Issues vs warnings |
| **Stats** | Commit count, size, changes/week | Per-agent and aggregate |
| **Schedule** | `cron` or `systemd` user timer | hourly/daily/weekly |
| **Hooks** | `pre-backup.d/`, `post-backup.d/`, `pre-restore.d/`, `post-restore.d/` | Executable scripts, alphabetical order |
| **Config** | `~/.config/asb/config` (bash-sourced) | `ASB_BACKUP_ROOT`, `ASB_AUTO_COMMIT`, `ASB_VERBOSE` |
| **JSON output** | `--json` flag | All commands support it |
| **TOON output** | `--format toon` | Requires external `toon.sh` |
| **Shell completion** | bash, zsh, fish | Full completion with dynamic commit/tag lookup |
| **Dry-run** | `--dry-run` / `-n` | All operations |
| **Force** | `--force` / `-f` | Skip confirmations |

### What We Found On User's Machine (6 agents installed)

| Agent | Location(s) | Key Files |
|-------|-------------|-----------|
| **Claude Code** | `~/.claude/` + `~/.local/share/claude/` + `~/.claude.json` | `settings.json`, hooks/, skills/, projects/*.jsonl, transcripts/, plans/, tasks/, `~/.claude.json` (48KB rich config with userID, mcpServers, githubRepoPaths, projects, skillUsage) |
| **Codex** | `~/.codex/` | `config.toml`, `auth.json`, `hooks.json`, `config.json`, memories/ (MEMORY.md + rollout_summaries), skills/, history.jsonl, session_index.jsonl, logs_2.sqlite, state_5.sqlite |
| **Cursor** | `~/.cursor/` | `hooks.json`, `mcp.json`, skills/ (100+ SKILL.md files), hooks/ |
| **Gemini** | `~/.gemini/` | `settings.json`, `trustedFolders.json`, `projects.json`, history/, skills/ (100+ SKILL.md files) |
| **OpenCode** | `~/.config/opencode/` + `~/.local/share/opencode/` | `opencode.json`, `oh-my-openagent.json`, `dcp.jsonc`, `opencode.db` (SQLite), storage/, log/ |
| **Kiro** | `~/.kiro/` | `settings/cli.json`, `settings/feed_state.json`, sessions/cli/ (20+ session files), skills/, agents/ |

### What `asb` MISSES (our Rust port must fix)

1. **Multi-location agents**: Claude has 3 locations (`~/.claude/`, `~/.local/share/claude/`, `~/.claude.json`); OpenCode has 2 locations (`~/.config/opencode/`, `~/.local/share/opencode/`)
2. **XDG compliance**: `~/.config/` and `~/.local/share/` locations not covered
3. **Kiro entirely missing** from asb's agent list
4. **SQLite databases**: `*.sqlite` files excluded by asb (only excludes `*.sqlite3-wal`/`*.sqlite3-shm`), but these are critical state files
5. **Memories**: Codex `memories/` directory with MEMORY.md
6. **Transcripts**: Claude `transcripts/` directory
7. **MCP configs**: `mcp.json` files across agents
8. **Session files**: Kiro sessions/, Codex history.jsonl

---

## 2. RUST PORT ARCHITECTURE

### Crate Structure

```
coding_agent_settings_backup/
├── Cargo.toml
├── src/
│   ├── main.rs          # CLI entry point, arg parsing
│   ├── config.rs        # Config loading (TOML), env vars, defaults
│   ├── agent.rs         # Agent trait, definitions, discovery
│   ├── backup.rs        # Backup operations (sync, git, commit)
│   ├── restore.rs       # Restore operations (preview, confirm, sync)
│   ├── export.rs        # Export/import (tar.gz, pipe)
│   ├── history.rs       # Git log, tags, history
│   ├── diff.rs          # Current vs backup comparison
│   ├── verify.rs        # Git fsck, integrity checks
│   ├── stats.rs         # Statistics computation
│   ├── schedule.rs      # Cron/systemd scheduling
│   ├── hooks.rs         # Hook execution system
│   ├── completion.rs    # Shell completion generators
│   ├── output.rs        # Human-readable + JSON + TOON output
│   └── filter.rs        # Smart exclusion/filtering rules
```

### Dependencies

```toml
[dependencies]
clap = { version = "4", features = ["derive", "complete"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tracing = "0.1"
tracing-subscriber = "0.3"
walkdir = "2"        # Directory traversal
ignore = "0.4"       # .gitignore-style filtering
tar = "0.4"          # Archive creation
flate2 = "1"         # gzip compression
chrono = "0.4"       # Date/time
dirs = "5"           # Home dir, XDG dirs
colored = "2"        # Terminal colors
```

### Design Decisions

**No `git2` crate** — Use `std::process::Command` to call `git` CLI directly. Reasons:
- `git2` requires building `libgit2` (C library) → slow compile, ~2MB larger binary
- We only need `git add`, `git commit`, `git log`, `git tag`, `git fsck`, `git diff` — all trivial via CLI
- CLI approach is simpler, more portable, and matches how `asb` already works

**No `nix` crate** — Use `std::process::Command` for hook execution. Reasons:
- `nix` is Linux/macOS only; `std::process::Command` works on Windows too
- Hook execution is just running an executable with env vars — `Command` handles this natively

---

## 3. AGENT DEFINITION SYSTEM

### Agent Trait

```rust
pub struct AgentConfig {
    pub key: String,              // e.g., "claude"
    pub display_name: String,     // e.g., "Claude Code"
    pub locations: Vec<AgentLocation>,  // Multi-location support
    pub is_installed: bool,       // Computed at runtime
    pub category: AgentCategory,  // coding, assistant, ide, etc.
}

pub struct AgentLocation {
    pub path: PathBuf,            // Absolute path
    pub location_type: LocationType, // HomeDir, XdgConfig, XdgData, Custom
    pub required: bool,           // Must exist for agent to be "installed"
}

pub enum LocationType {
    HomeDir,      // ~/.something
    XdgConfig,    // ~/.config/something
    XdgData,      // ~/.local/share/something
    Custom,       // User-defined
}
```

### Built-in Agents (expanded from asb + our discoveries)

| Key | Display Name | Locations |
|-----|-------------|-----------|
| `claude` | Claude Code | `~/.claude/` (HomeDir), `~/.local/share/claude/` (XdgData), `~/.claude.json` (HomeDir, file) |
| `codex` | OpenAI Codex CLI | `~/.codex/` (HomeDir) |
| `cursor` | Cursor | `~/.cursor/` (HomeDir) |
| `gemini` | Google Gemini | `~/.gemini/` (HomeDir) |
| `cline` | Cline | `~/.cline/` (HomeDir) |
| `amp` | Amp (Sourcegraph) | `~/.amp/` (HomeDir) |
| `aider` | Aider | `~/.aider/` (HomeDir) |
| `opencode` | OpenCode | `~/.config/opencode/` (XdgConfig), `~/.local/share/opencode/` (XdgData) |
| `factory` | Factory Droid | `~/.factory/` (HomeDir) |
| `windsurf` | Windsurf | `~/.windsurf/` (HomeDir) |
| `plandex` | Plandex | `~/.plandex-home/` (HomeDir) |
| `qwencode` | Qwen Code | `~/.qwen/` (HomeDir) |
| `amazonq` | Amazon Q | `~/.q/` (HomeDir) |
| `kiro` | Kiro (NEW) | `~/.kiro/` (HomeDir) |
| `continue` | Continue (NEW) | `~/.continue/` (HomeDir) |
| `copilot` | GitHub Copilot CLI (NEW) | `~/.copilot/` (HomeDir) |
| `zed` | Zed Editor (NEW) | `~/.zed/` (HomeDir) |
| `roo` | Roo Code (NEW) | `~/.roo/` (HomeDir) |
| `trae` | Trae (NEW) | `~/.trae/` (HomeDir) |

### Discovery System

Scan `~/.` for dot-directories matching known agent patterns. Check against existing agents. Present new findings for interactive or `--auto` addition.

---

## 4. AGENT FOLDER STRUCTURES (Real System — Verified)

This section documents the **exact folder/file structure** for each of the 6 agents discovered on the user's machine. This is the source of truth for what the backup tool must capture.

### 4.1 Claude Code — 3 Locations

**Location A: `~/.claude/`** (main config dir)
```
~/.claude/
├── settings.json              # Main settings: env vars, permissions, model, features
├── hooks/
│   └── dcg-pre-shell.py       # Pre-shell destructive command guard
├── skills/
│   └── review-work/
│       └── SKILL.md           # Skill definition files
├── projects/
│   └── project-alpha.jsonl    # Per-project session data (JSONL)
├── transcripts/
│   └── session-20260520.json  # Full session transcripts
├── paste-cache/               # Clipboard cache (empty or transient)
├── plans/
│   └── dark-mode-implementation.md  # Implementation plans
├── tasks/
│   └── task-001.json          # Task tracking
└── skill-learning/            # Skill learning data
```

**Location B: `~/.local/share/claude/`** (XDG data dir)
```
~/.local/share/claude/
└── versions/
    └── current.json           # Version info, build date, channel
```

**Location C: `~/.claude.json`** (root-level rich config, ~48KB)
```
~/.claude.json                 # Rich config: userID, mcpServers, githubRepoPaths,
                               # projects (per-project model/permissions), skillUsage,
                               # autoUpdates, featureFlags, completion, theme, telemetry
```

**Key observations**:
- `settings.json` contains API keys in `env` block → must backup
- `~/.claude.json` is the richest single file → MCP server configs, project metadata
- `transcripts/` contains full session logs → can be large
- `paste-cache/` is transient → should be excluded

---

### 4.2 Codex — Single Location

**Location: `~/.codex/`**
```
~/.codex/
├── config.toml                # Profile configs, provider settings, feature flags
├── auth.json                  # Auth provider, API key hint, refresh token
├── hooks.json                 # Hook definitions (pre/post tool use, session start)
├── config.json                # Version, installation_id, telemetry, preferences
├── version.json               # Version, commit hash, build date, channel
├── installation_id            # Single-line installation identifier
├── history.jsonl              # Session message history (JSONL)
├── session_index.jsonl        # Session index with metadata
├── external_agent_session_imports.jsonl  # Imported sessions from other agents
├── memories/
│   ├── MEMORY.md              # Persistent project memory
│   └── rollout_summaries/
│       └── 2026-05-20.md      # Session rollout summaries
├── skills/
│   └── git-master/
│       └── SKILL.md           # Skill definitions (100+ in reality)
└── shell_snapshots/
    └── snapshot-20260520.json # Shell environment snapshots
```

**Missing from mock**: `logs_2.sqlite`, `state_5.sqlite` (Codex state databases)

**Key observations**:
- `config.toml` has multiple provider sections (openai, anthropic) with API keys
- `auth.json` contains refresh tokens → sensitive
- SQLite DBs are state files → MUST backup (only WAL/SHM excluded)
- `memories/MEMORY.md` is user-curated knowledge → important

---

### 4.3 Cursor — Single Location

**Location: `~/.cursor/`**
```
~/.cursor/
├── hooks.json                 # Hook definitions (pre-shell, on-file-change)
├── mcp.json                   # MCP server configurations
├── hooks/
│   └── dcg-pre-shell.py       # Pre-shell destructive command guard
└── skills/
    ├── review-work/
    │   └── SKILL.md
    ├── codebase-archaeology/
    │   └── SKILL.md
    ├── git-master/
    │   └── SKILL.md
    ├── simplify-and-refactor-code-isomorphically/
    │   └── SKILL.md
    └── ... (100+ more SKILL.md files in reality)
```

**Key observations**:
- `mcp.json` mirrors Claude's MCP config format
- Skills directory is large (100+) → backup all, no exclusions needed
- Hook scripts are Python → must preserve execute permissions

---

### 4.4 Gemini — Single Location

**Location: `~/.gemini/`**
```
~/.gemini/
├── settings.json              # Model, API key, generation config, sandbox, telemetry
├── trustedFolders.json        # Trusted project folders with timestamps
├── projects.json              # Per-project metadata (name, lastOpened, model)
├── history/
│   └── session-20260520.json  # Session history records
└── skills/
    ├── security-audit-for-saas/
    │   └── SKILL.md
    ├── supabase/
    │   └── SKILL.md
    ├── vercel/
    │   └── SKILL.md
    └── ... (100+ more SKILL.md files in reality)
```

**Key observations**:
- `settings.json` contains `apiKey` directly → sensitive
- `trustedFolders.json` tracks which projects the user trusts
- Skills directory is large (100+) → backup all

---

### 4.5 OpenCode — 2 Locations

**Location A: `~/.config/opencode/`** (XDG config dir)
```
~/.config/opencode/
├── opencode.json              # Main config: theme, model, provider, API key, MCP servers
├── oh-my-openagent.json       # Agent orchestration config: models, parallelism, timeouts
└── dcp.jsonc                  # Destructive Command Guard config (JSONC with comments)
```

**Location B: `~/.local/share/opencode/`** (XDG data dir)
```
~/.local/share/opencode/
├── opencode.db                # SQLite database (main state store)
├── storage/
│   └── session-cache.json     # Session cache with metadata
├── log/
│   └── opencode.log           # Application log file
└── tool-output/
    └── last-explorer-result.json  # Last tool execution output
```

**Key observations**:
- `opencode.db` is the primary state database → MUST backup
- `dcp.jsonc` uses JSONC format (with comments) → parser must handle comments
- `opencode.log` is transient → should be excluded from backup
- Two XDG locations must be merged into single backup repo

---

### 4.6 Kiro — Single Location (NEW — not in asb)

**Location: `~/.kiro/`**
```
~/.kiro/
├── settings/
│   ├── cli.json               # CLI settings: model, provider, API key, features
│   └── feed_state.json        # Feed state: lastUpdate, seenFeatures, usageStats
├── sessions/
│   └── cli/
│       ├── session-20260521-001.json   # Session metadata
│       ├── session-20260521-001.jsonl  # Session messages (JSONL)
│       ├── session-20260522-001.json
│       ├── session-20260522-001.jsonl
│       ├── session-20260523-001.json
│       ├── session-20260523-001.jsonl
│       └── ... (20+ sessions in reality)
├── skills/
│   ├── codebase-audit/
│   │   └── SKILL.md
│   ├── testing-conformance-harnesses/
│   │   └── SKILL.md
│   └── ... (more skills)
├── agents/
│   └── agent-config.json      # Agent definitions with model/provider config
└── .cli_bash_history          # CLI bash command history
```

**Key observations**:
- Sessions come in pairs: `.json` (metadata) + `.jsonl` (messages)
- `feed_state.json` tracks user engagement stats
- `.cli_bash_history` is shell history → may contain sensitive commands
- Not in asb's agent list at all → must add as built-in

---

## 5. SMART FILTERING SYSTEM

### Default Exclusions (per-agent gitignore)

```
# Logs and temporary files
*.log
*.tmp
*.temp
*.swp
*~

# OS files
.DS_Store
Thumbs.db

# Large binary caches
**/cache/
**/Cache/
**/.cache/

# SQLite temp files (not the DB itself!)
*.sqlite3-wal
*.sqlite3-shm

# Agent-specific large/transient files
**/paste-cache/
**/logs_*.sqlite    # Codex state DBs - KEEP these, exclude only temp files
```

### Key Difference from asb

- **asb excludes `*.sqlite3-wal` and `*.sqlite3-shm`** (correct — these are WAL temp files)
- **asb does NOT exclude `*.sqlite`** — but the Rust port should be explicit: SQLite DBs ARE backed up, only WAL/SHM temp files are excluded
- **asb excludes `cache/`, `Cache/`, `.cache/`** — correct
- **asb excludes `*.log`** — correct

### Per-Agent Custom Exclusions

Allow agent-specific `.casbignore` files (like `.gitignore` but for casb) in each agent's backup directory.

---

## 6. CLI INTERFACE

### Command Structure (mirrors asb + improvements)

```
casb [OPTIONS] <COMMAND>

Global options:
  -n, --dry-run          Show what would happen without making changes
  -f, --force            Skip confirmation prompts
  -v, --verbose          Show detailed output
  -q, --quiet            Suppress non-error output
      --json             Output machine-readable JSON
      --format <FORMAT>  Output format: json | toon | text [default: text]

Commands:
  backup [AGENTS...]     Backup agent settings (all if none specified)
  restore <AGENT> [REF]  Restore agent from backup (commit hash or tag)
  export <AGENT> [FILE]  Export backup as tar.gz archive (- for stdout)
  import [FILE]          Import backup from archive (- for stdin)
  list                   List all agents and backup status
  history <AGENT>        Show backup history
  diff <AGENT>           Show changes since last backup
  verify [AGENTS...]     Verify backup integrity
  tag <AGENT> <NAME>     Tag a backup commit
  stats [AGENT]          Show backup statistics
  discover               Scan for new AI agents
  schedule               Manage automated backup schedules
  hooks                  List configured hooks
  init                   Initialize backup location
  config                 Manage configuration
  doctor                 Run health check (NEW)
  completion <SHELL>     Generate shell completion
  help                   Show help
  version                Show version
```

### NEW: `doctor` Command

Health check that verifies:
- git is installed and working
- rsync availability (with fallback info)
- backup root exists and is writable
- each installed agent's source is readable
- each backup repo is valid (git fsck)
- hooks directories exist
- config file is valid
- schedule status
- disk space check

---

## 7. CONFIGURATION (TOML instead of bash-sourced)

### Location: `~/.config/casb/config.toml`

```toml
[general]
backup_root = "~/.agent_settings_backups"  # or $CASB_BACKUP_ROOT
auto_commit = true
verbose = false
quiet = false
output_format = "text"  # text | json | toon

[backup]
# Default exclusions (applied to all agents)
exclusions = [
    "*.log", "*.tmp", "*.temp", "*.swp", "*~",
    ".DS_Store", "Thumbs.db",
    "**/cache/", "**/Cache/", "**/.cache/",
    "*.sqlite3-wal", "*.sqlite3-shm",
]
use_rsync = true  # false = always use cp fallback
checksum_verify = true  # verify file integrity during sync

[schedule]
# Default schedule settings
method = "systemd"  # systemd | cron | none
interval = "daily"  # hourly | daily | weekly

[agents.kiro]  # Custom agent example
enabled = true
display_name = "Kiro"
locations = ["~/.kiro/"]
```

### Environment Variables (override config)

| Variable | Default | Description |
|----------|---------|-------------|
| `CASB_BACKUP_ROOT` | `~/.agent_settings_backups` | Backup location |
| `CASB_AUTO_COMMIT` | `true` | Auto-commit on backup |
| `CASB_VERBOSE` | `false` | Verbose output |
| `CASB_CONFIG` | `~/.config/casb/config.toml` | Config file path |

---

## 8. BACKUP REPOSITORY STRUCTURE

Same as asb — each agent gets its own git repo:

```
~/.agent_settings_backups/
├── README.md
├── .claude/           # Git repo - Claude settings
│   ├── .git/
│   ├── .gitignore
│   ├── settings.json
│   ├── hooks/
│   ├── skills/
│   └── ...
├── .codex/            # Git repo - Codex settings
│   ├── .git/
│   ├── config.toml
│   ├── auth.json
│   └── ...
├── .kiro/             # Git repo - Kiro settings (NEW)
│   ├── .git/
│   ├── settings/
│   ├── sessions/
│   └── ...
└── ...
```

### Multi-location Backup Strategy

For agents with multiple locations (Claude, OpenCode):
- **Single backup repo per agent** (not per location)
- Locations are merged into the backup repo with subdirectory prefixes:
  ```
  ~/.agent_settings_backups/.claude/
  ├── home/          # From ~/.claude/
  │   ├── settings.json
  │   └── ...
  ├── data/          # From ~/.local/share/claude/
  │   └── versions/
  └── claude.json    # From ~/.claude.json (root-level file)
  ```
- OR: flat merge with collision detection (prefer simpler approach)

**Decision**: Use flat merge with collision detection. If two locations have the same relative path, suffix with location identifier. This keeps the structure simple and searchable.

---

## 9. IMPLEMENTATION PHASES

### Phase 1: Core Infrastructure
1. Project scaffold with `clap` CLI
2. Agent definition system (trait + built-in agents)
3. Config loading (TOML + env vars)
4. Output system (text + JSON)
5. `init`, `version`, `help` commands

### Phase 2: Backup & Restore
6. Git operations (init, add, commit, log)
7. File sync (rsync with cp fallback)
8. `backup` command (single + all agents)
9. `restore` command (preview + confirm + sync)
10. `list` command

### Phase 3: Advanced Features
11. `history` command
12. `diff` command
13. `tag` command (create, list, delete)
14. `verify` command
15. `stats` command

### Phase 4: Portability & Automation
16. `export` / `import` commands
17. `schedule` command (cron + systemd)
18. `hooks` system
19. `discover` command
20. Shell completion (bash, zsh, fish)

### Phase 5: Polish
21. `doctor` command
22. Multi-location agent support
23. Custom agents config
24. TOON output support
25. `casbignore` per-agent exclusions
26. Installer script

---

## 10. KEY IMPROVEMENTS OVER ASB

| Feature | asb (bash) | casb (Rust) |
|---------|-----------|-------------|
| **Multi-location** | ❌ Single location per agent | ✅ Multiple locations merged |
| **Kiro support** | ❌ Missing | ✅ Built-in |
| **Config format** | Bash-sourced (unsafe) | TOML (safe, structured) |
| **SQLite handling** | Implicit exclusion | Explicit: DBs kept, WAL/SHM excluded |
| **Type safety** | None | Full Rust type safety |
| **Error handling** | Exit codes only | Structured errors with context |
| **Parallel backup** | Sequential | ✅ Parallel agent backup |
| **Doctor command** | ❌ | ✅ Health check |
| **Binary size** | 4,083 lines bash | ~5-8MB static binary (no libgit2) |
| **Dependencies** | git, rsync (optional) | `git` CLI required (already installed) |
| **Cross-platform** | Linux/macOS only | Linux/macOS/Windows |
| **Git operations** | External git calls | `std::process::Command` → git CLI |
| **Hook execution** | External shell scripts | `std::process::Command` (cross-platform) |

---

## 11. RISK ASSESSMENT

| Risk | Impact | Mitigation |
|------|--------|------------|
| `git` CLI not available | Low | Check in `doctor` command, fail early with clear message |
| rsync not available | Low | cp fallback (same as asb) |
| Multi-location merge conflicts | Medium | Collision detection + suffix strategy |
| Large backup sizes | Low | Exclusion filters + git gc |
| Hook security | Medium | Hooks run as current user, no sudo |
| Config migration from asb | Low | Import existing `~/.config/asb/config` on first run |
| `git` version differences | Low | Use stable subcommands, test against git 2.30+ |
