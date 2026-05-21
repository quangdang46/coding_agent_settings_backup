#!/usr/bin/env bash
# generate_mock_agents.sh — Generate realistic mock agent config folders for development/testing
# All sensitive data (API keys, model names, credentials, user IDs) is replaced with safe placeholders.
#
# Usage:
#   ./scripts/generate_mock_agents.sh /tmp/mock_agents
#   ./scripts/generate_mock_agents.sh /tmp/mock_agents --agent claude
#   ./scripts/generate_mock_agents.sh /tmp/mock_agents --all

set -euo pipefail

OUTPUT_DIR="${1:?Usage: $0 <output_dir> [--agent <name>] [--all]}"
shift || true

# Parse options
GENERATE_ALL=false
SPECIFIC_AGENT=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --agent)
            SPECIFIC_AGENT="$2"
            shift 2
            ;;
        --all)
            GENERATE_ALL=true
            shift
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

should_generate() {
    local name="$1"
    if [[ "$GENERATE_ALL" == "true" ]]; then
        return 0
    fi
    if [[ -n "$SPECIFIC_AGENT" && "$SPECIFIC_AGENT" == "$name" ]]; then
        return 0
    fi
    if [[ -z "$SPECIFIC_AGENT" && "$GENERATE_ALL" == "false" ]]; then
        # Default: generate all if no specific agent requested
        return 0
    fi
    return 1
}

echo "Generating mock agent configs in: $OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

###############################################################################
# 1. CLAUDE CODE — ~/.claude/ + ~/.claude.json + ~/.local/share/claude/
###############################################################################
generate_claude() {
    local base="$OUTPUT_DIR/.claude"
    local home_claude="$base/home"
    local data_claude="$base/data"
    local root_config="$base/root_config"

    mkdir -p "$home_claude"/{hooks,"skills/review-work",projects,transcripts,paste-cache,plans,tasks,skill-learning}
    mkdir -p "$data_claude/versions"
    mkdir -p "$root_config"

    # ~/.claude/settings.json
    cat > "$home_claude/settings.json" << 'EOF'
{
  "env": {
    "ANTHROPIC_API_KEY": "[REDACTED]",
    "OPENAI_API_KEY": "[REDACTED]",
    "GEMINI_API_KEY": "[REDACTED]"
  },
  "permissions": {
    "allow": [
      "Read(*)",
      "Edit(*)",
      "Bash(*)"
    ],
    "deny": []
  },
  "model": "[REDACTED_MODEL_NAME]",
  "quality": "high",
  "autoApprove": [],
  "bashStrict": true,
  "checkpoints": true,
  "fileFiltering": {
    "respectGitIgnore": true,
    "respectGeminiIgnore": false,
    "respectClaudeIgnore": true,
    "respectOpenCodeIgnore": false,
    "respectCursorIgnore": false,
    "respectKiroIgnore": false
  },
  "includeCoAuthoredBy": true,
  "enableAllProjectMcpServers": false,
  "allowedTools": [],
  "hasCompletedOnboarding": true,
  "lastOnboardingVersion": "1.0.0"
}
EOF

    # ~/.claude.json (the rich 48KB config — sanitized)
    cat > "$root_config/claude.json" << 'EOF'
{
  "userID": "[REDACTED_USER_ID]",
  "mcpServers": {
    "frankensearch": {
      "command": "npx",
      "args": ["-y", "@anthropic-ai/mcp-server-frankensearch"],
      "env": {},
      "disabled": false,
      "alwaysAllow": []
    },
    "context7": {
      "command": "npx",
      "args": ["-y", "@anthropic-ai/mcp-server-context7"],
      "env": {},
      "disabled": false,
      "alwaysAllow": []
    },
    "playwright": {
      "command": "npx",
      "args": ["-y", "@anthropic-ai/mcp-server-playwright"],
      "env": {},
      "disabled": false,
      "alwaysAllow": []
    }
  },
  "githubRepoPaths": [
    "https://github.com/[REDACTED_ORG]/project-alpha",
    "https://github.com/[REDACTED_ORG]/project-beta",
    "https://github.com/[REDACTED_ORG]/project-gamma"
  ],
  "projects": {
    "/data/projects/project-alpha": {
      "name": "project-alpha",
      "lastOpened": "2026-05-20T10:30:00Z",
      "model": "[REDACTED_MODEL_NAME]",
      "permissionMode": "defaultAccept"
    },
    "/data/projects/project-beta": {
      "name": "project-beta",
      "lastOpened": "2026-05-19T15:45:00Z",
      "model": "[REDACTED_MODEL_NAME]",
      "permissionMode": "suggest"
    }
  },
  "skillUsage": [
    {
      "skill": "review-work",
      "lastUsed": "2026-05-20T10:30:00Z",
      "usageCount": 42
    },
    {
      "skill": "codebase-archaeology",
      "lastUsed": "2026-05-18T09:00:00Z",
      "usageCount": 15
    }
  ],
  "autoUpdates": {
    "enabled": true,
    "lastCheck": "2026-05-20T00:00:00Z"
  },
  "featureFlags": {
    "newUI": true,
    "experimentalTools": false,
    "betaFeatures": true
  },
  "completion": {
    "enabled": true,
    "model": "[REDACTED_MODEL_NAME]"
  },
  "theme": "dark",
  "telemetry": {
    "enabled": false,
    "sessionId": "[REDACTED_SESSION_ID]"
  }
}
EOF

    # ~/.claude/hooks/ — sample hook
    cat > "$home_claude/hooks/dcg-pre-shell.py" << 'EOF'
#!/usr/bin/env python3
"""Pre-shell hook: Destructive Command Guard for Claude Code."""
import sys
import json

DANGEROUS_PATTERNS = [
    "rm -rf /",
    "git reset --hard",
    "git clean -fd",
    "DROP DATABASE",
    "kubectl delete",
]

def check_command(cmd):
    for pattern in DANGEROUS_PATTERNS:
        if pattern.lower() in cmd.lower():
            return False, f"Blocked: {pattern}"
    return True, "OK"

if __name__ == "__main__":
    if len(sys.argv) > 1:
        cmd = sys.argv[1]
        allowed, msg = check_command(cmd)
        if not allowed:
            print(json.dumps({"allowed": False, "reason": msg}))
            sys.exit(1)
    print(json.dumps({"allowed": True}))
EOF
    chmod +x "$home_claude/hooks/dcg-pre-shell.py"

    # ~/.claude/skills/ — sample skill
    cat > "$home_claude/skills/review-work/SKILL.md" << 'EOF'
# review-work

Post-implementation review orchestrator. Launches 5 parallel background sub-agents:
Oracle (goal/constraint verification), Oracle (code quality), Oracle (security),
unspecified-high (hands-on QA execution), unspecified-high (context mining).

All must pass for review to pass.

## Trigger
'review work', 'review my work', 'review changes', 'QA my work',
'verify implementation', 'check my work', 'validate changes',
'post-implementation review'
EOF

    # ~/.claude/projects/*.jsonl
    cat > "$home_claude/projects/project-alpha.jsonl" << 'EOF'
{"type":"message","role":"user","content":"Add dark mode toggle","timestamp":"2026-05-20T10:30:00Z"}
{"type":"message","role":"assistant","content":"I'll add a dark mode toggle...","timestamp":"2026-05-20T10:30:15Z"}
{"type":"tool_use","name":"edit","content":"Modified theme config","timestamp":"2026-05-20T10:31:00Z"}
EOF

    # ~/.claude/transcripts/
    cat > "$home_claude/transcripts/session-20260520.json" << 'EOF'
{
  "sessionId": "[REDACTED_SESSION_ID]",
  "startTime": "2026-05-20T10:30:00Z",
  "endTime": "2026-05-20T11:45:00Z",
  "project": "/data/projects/project-alpha",
  "model": "[REDACTED_MODEL_NAME]",
  "messageCount": 45,
  "toolCalls": 12,
  "tokensUsed": {
    "input": 125000,
    "output": 35000
  }
}
EOF

    # ~/.claude/plans/
    cat > "$home_claude/plans/dark-mode-implementation.md" << 'EOF'
# Dark Mode Implementation Plan

## Phase 1: Infrastructure
- [x] Add theme context provider
- [x] Create CSS variable system
- [ ] Add toggle component

## Phase 2: Component Updates
- [ ] Update all components to use CSS variables
- [ ] Test with screen readers

## Phase 3: Polish
- [ ] Add transition animations
- [ ] Persist user preference
EOF

    # ~/.claude/tasks/
    cat > "$home_claude/tasks/task-001.json" << 'EOF'
{
  "id": "task-001",
  "title": "Implement dark mode toggle",
  "status": "in_progress",
  "priority": "high",
  "created": "2026-05-20T10:30:00Z",
  "assignee": "[REDACTED_AGENT_ID]"
}
EOF

    # ~/.local/share/claude/versions/
    cat > "$data_claude/versions/current.json" << 'EOF'
{
  "version": "1.0.0",
  "buildDate": "2026-05-15",
  "channel": "stable",
  "lastUpdateCheck": "2026-05-20T00:00:00Z"
}
EOF

    echo "  ✓ Claude Code mock generated"
}

###############################################################################
# 2. CODEX — ~/.codex/
###############################################################################
generate_codex() {
    local base="$OUTPUT_DIR/.codex"
    mkdir -p "$base"/{memories/rollout_summaries,"skills/git-master",shell_snapshots}

    # ~/.codex/config.toml
    cat > "$base/config.toml" << 'EOF'
# Codex CLI Configuration

[profile.default]
model = "[REDACTED_MODEL_NAME]"
approval_policy = "suggest"
sandbox_mode = "read-only"

[profile.work]
model = "[REDACTED_MODEL_NAME]"
approval_policy = "auto-edit"
sandbox_mode = "full"

[providers.openai]
api_key = "[REDACTED]"
base_url = "https://api.openai.com/v1"

[providers.anthropic]
api_key = "[REDACTED]"
base_url = "https://api.anthropic.com"

[features]
memory_enabled = true
skill_system = true
hooks_enabled = true
EOF

    # ~/.codex/auth.json
    cat > "$base/auth.json" << 'EOF'
{
  "provider": "openai",
  "auth_type": "api_key",
  "api_key_hint": "sk-[REDACTED]...[REDACTED_LAST4]",
  "token_expiry": null,
  "refresh_token": "[REDACTED]",
  "last_authenticated": "2026-05-20T00:00:00Z"
}
EOF

    # ~/.codex/hooks.json
    cat > "$base/hooks.json" << 'EOF'
{
  "hooks": {
    "pre_tool_use": {
      "enabled": true,
      "scripts": ["~/.codex/hooks/pre-tool.sh"]
    },
    "post_tool_use": {
      "enabled": false,
      "scripts": []
    },
    "on_session_start": {
      "enabled": true,
      "scripts": ["~/.codex/hooks/session-init.sh"]
    }
  }
}
EOF

    # ~/.codex/config.json
    cat > "$base/config.json" << 'EOF'
{
  "version": "0.1.0",
  "installation_id": "[REDACTED_INSTALLATION_ID]",
  "telemetry": {
    "enabled": false,
    "session_id": "[REDACTED_SESSION_ID]"
  },
  "preferences": {
    "theme": "dark",
    "auto_save": true,
    "max_context_tokens": 128000
  }
}
EOF

    # ~/.codex/memories/MEMORY.md
    cat > "$base/memories/MEMORY.md" << 'EOF'
# Project Memory

## Architecture Decisions
- Using Rust for CLI tools, Next.js for web
- SQLite for local storage, Supabase for cloud
- Agent coordination via MCP Agent Mail

## Key Patterns
- Always use `rch exec --` for cargo builds
- Never delete files without permission
- Main branch only, no master references

## Recent Context
- Working on coding_agent_settings_backup port
- 6 agents discovered on system
- Multi-location support needed for Claude and OpenCode
EOF

    # ~/.codex/memories/rollout_summaries/
    cat > "$base/memories/rollout_summaries/2026-05-20.md" << 'EOF'
# Rollout Summary — 2026-05-20

## Session: asb Rust port planning
- Researched original bash script (4,083 lines)
- Discovered 6 installed agents on system
- Created detailed Rust port plan
- Identified multi-location gaps in original
EOF

    # ~/.codex/skills/
    cat > "$base/skills/git-master/SKILL.md" << 'EOF'
# git-master

MUST USE for ANY git operations. Atomic commits, rebase/squash, history search.

## Triggers
'commit', 'rebase', 'squash', 'who wrote', 'when was X added', 'find the commit that'
EOF

    # ~/.codex/history.jsonl
    cat > "$base/history.jsonl" << 'EOF'
{"type":"session_start","timestamp":"2026-05-20T10:30:00Z","model":"[REDACTED]","project":"/data/projects/coding_agent_settings_backup"}
{"type":"message","role":"user","timestamp":"2026-05-20T10:30:01Z","content":"Port asb to Rust"}
{"type":"message","role":"assistant","timestamp":"2026-05-20T10:30:15Z","content":"I'll research the original script first..."}
{"type":"tool_use","timestamp":"2026-05-20T10:31:00Z","tool":"bash","content":"ls -la /tmp/agent_settings_backup_script/"}
{"type":"session_end","timestamp":"2026-05-20T11:45:00Z","tokens":{"input":125000,"output":35000}}
EOF

    # ~/.codex/session_index.jsonl
    cat > "$base/session_index.jsonl" << 'EOF'
{"session_id":"[REDACTED]","start":"2026-05-20T10:30:00Z","end":"2026-05-20T11:45:00Z","project":"/data/projects/coding_agent_settings_backup","messages":45}
{"session_id":"[REDACTED]","start":"2026-05-19T14:00:00Z","end":"2026-05-19T15:30:00Z","project":"/data/projects/project-alpha","messages":32}
EOF

    # ~/.codex/external_agent_session_imports.jsonl
    cat > "$base/external_agent_session_imports.jsonl" << 'EOF'
{"source":"claude","session_id":"[REDACTED]","imported_at":"2026-05-20T10:00:00Z","messages":28}
EOF

    # ~/.codex/version.json
    cat > "$base/version.json" << 'EOF'
{
  "version": "0.1.0",
  "commit": "[REDACTED_GIT_HASH]",
  "build_date": "2026-05-15",
  "channel": "stable"
}
EOF

    # ~/.codex/installation_id
    echo "[REDACTED_INSTALLATION_ID]" > "$base/installation_id"

    # ~/.codex/logs_2.sqlite (mock SQLite database)
    # Create a minimal valid SQLite file with a simple table
    python3 -c "
import sqlite3, os
db_path = '$base/logs_2.sqlite'
conn = sqlite3.connect(db_path)
conn.execute('CREATE TABLE IF NOT EXISTS log_entries (id INTEGER PRIMARY KEY, timestamp TEXT, level TEXT, message TEXT)')
conn.execute(\"INSERT INTO log_entries (timestamp, level, message) VALUES ('2026-05-20T10:30:00Z', 'INFO', 'Session started')\")
conn.execute(\"INSERT INTO log_entries (timestamp, level, message) VALUES ('2026-05-20T10:31:00Z', 'DEBUG', 'Tool call: explore')\")
conn.execute(\"INSERT INTO log_entries (timestamp, level, message) VALUES ('2026-05-20T11:45:00Z', 'INFO', 'Session ended')\")
conn.commit()
conn.close()
" 2>/dev/null || touch "$base/logs_2.sqlite"

    # ~/.codex/state_5.sqlite (mock SQLite database)
    python3 -c "
import sqlite3
db_path = '$base/state_5.sqlite'
conn = sqlite3.connect(db_path)
conn.execute('CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT, updated_at TEXT)')
conn.execute(\"INSERT INTO state (key, value, updated_at) VALUES ('last_session_id', '[REDACTED_SESSION_ID]', '2026-05-20T11:45:00Z')\")
conn.execute(\"INSERT INTO state (key, value, updated_at) VALUES ('total_sessions', '156', '2026-05-20T11:45:00Z')\")
conn.commit()
conn.close()
" 2>/dev/null || touch "$base/state_5.sqlite"

    # ~/.codex/shell_snapshots/
    cat > "$base/shell_snapshots/snapshot-20260520.json" << 'EOF'
{
  "timestamp": "2026-05-20T10:30:00Z",
  "cwd": "/data/projects/coding_agent_settings_backup",
  "env_vars": {
    "PATH": "[REDACTED]",
    "HOME": "/home/[REDACTED_USER]",
    "EDITOR": "nvim"
  },
  "git_branch": "main",
  "git_status": "clean"
}
EOF

    echo "  ✓ Codex mock generated"
}

###############################################################################
# 3. CURSOR — ~/.cursor/
###############################################################################
generate_cursor() {
    local base="$OUTPUT_DIR/.cursor"
    mkdir -p "$base"/{hooks,"skills/review-work","skills/codebase-archaeology","skills/git-master","skills/simplify-and-refactor-code-isomorphically"}

    # ~/.cursor/hooks.json
    cat > "$base/hooks.json" << 'EOF'
{
  "hooks": {
    "pre_shell_command": {
      "enabled": true,
      "script": "~/.cursor/hooks/dcg-pre-shell.py"
    },
    "on_file_change": {
      "enabled": false,
      "script": null
    }
  }
}
EOF

    # ~/.cursor/hooks/dcg-pre-shell.py
    cat > "$base/hooks/dcg-pre-shell.py" << 'EOF'
#!/usr/bin/env python3
"""Pre-shell hook for Cursor: blocks destructive commands."""
import sys, json

DANGEROUS = ["rm -rf /", "git reset --hard", "git clean -fd"]

def check(cmd):
    for pattern in DANGEROUS:
        if pattern.lower() in cmd.lower():
            return False, f"Blocked: {pattern}"
    return True, "OK"

if __name__ == "__main__":
    if len(sys.argv) > 1:
        allowed, msg = check(sys.argv[1])
        if not allowed:
            print(json.dumps({"allowed": False, "reason": msg}))
            sys.exit(1)
    print(json.dumps({"allowed": True}))
EOF
    chmod +x "$base/hooks/dcg-pre-shell.py"

    # ~/.cursor/mcp.json
    cat > "$base/mcp.json" << 'EOF'
{
  "mcpServers": {
    "frankensearch": {
      "command": "npx",
      "args": ["-y", "@anthropic-ai/mcp-server-frankensearch"],
      "disabled": false
    },
    "context7": {
      "command": "npx",
      "args": ["-y", "@anthropic-ai/mcp-server-context7"],
      "disabled": false
    }
  }
}
EOF

    # ~/.cursor/skills/ — representative sample (not 100+, just enough for testing)
    for skill in review-work codebase-archaeology git-master simplify-and-refactor-code-isomorphically; do
        mkdir -p "$base/skills/$skill"
        cat > "$base/skills/$skill/SKILL.md" << EOF
# $skill

Auto-generated mock skill for development testing.

## Description
Mock skill file for testing the coding_agent_settings_backup tool.

## Trigger
'test trigger for $skill'
EOF
    done

    echo "  ✓ Cursor mock generated"
}

###############################################################################
# 4. GEMINI — ~/.gemini/
###############################################################################
generate_gemini() {
    local base="$OUTPUT_DIR/.gemini"
    mkdir -p "$base"/{history,"skills/security-audit-for-saas","skills/supabase","skills/vercel"}

    # ~/.gemini/settings.json
    cat > "$base/settings.json" << 'EOF'
{
  "model": "[REDACTED_MODEL_NAME]",
  "apiKey": "[REDACTED]",
  "generationConfig": {
    "temperature": 0.7,
    "topP": 0.95,
    "topK": 40,
    "maxOutputTokens": 8192
  },
  "sandbox": {
    "enabled": true,
    "mode": "auto"
  },
  "telemetry": {
    "enabled": false,
    "sessionId": "[REDACTED_SESSION_ID]"
  },
  "theme": "dark",
  "includeCoAuthoredBy": true
}
EOF

    # ~/.gemini/trustedFolders.json
    cat > "$base/trustedFolders.json" << 'EOF'
{
  "trustedFolders": [
    {
      "path": "/data/projects/project-alpha",
      "trusted": true,
      "addedAt": "2026-05-10T00:00:00Z"
    },
    {
      "path": "/data/projects/project-beta",
      "trusted": true,
      "addedAt": "2026-05-15T00:00:00Z"
    }
  ]
}
EOF

    # ~/.gemini/projects.json
    cat > "$base/projects.json" << 'EOF'
{
  "projects": {
    "/data/projects/project-alpha": {
      "name": "project-alpha",
      "lastOpened": "2026-05-20T10:30:00Z",
      "model": "[REDACTED_MODEL_NAME]"
    },
    "/data/projects/project-beta": {
      "name": "project-beta",
      "lastOpened": "2026-05-19T15:45:00Z",
      "model": "[REDACTED_MODEL_NAME]"
    }
  }
}
EOF

    # ~/.gemini/history/
    cat > "$base/history/session-20260520.json" << 'EOF'
{
  "sessionId": "[REDACTED_SESSION_ID]",
  "startTime": "2026-05-20T10:30:00Z",
  "endTime": "2026-05-20T11:45:00Z",
  "project": "/data/projects/project-alpha",
  "model": "[REDACTED_MODEL_NAME]",
  "messageCount": 32
}
EOF

    # ~/.gemini/skills/ — representative sample
    for skill in security-audit-for-saas supabase vercel; do
        mkdir -p "$base/skills/$skill"
        cat > "$base/skills/$skill/SKILL.md" << EOF
# $skill

Auto-generated mock skill for development testing.

## Description
Mock skill file for testing the coding_agent_settings_backup tool.

## Trigger
'test trigger for $skill'
EOF
    done

    echo "  ✓ Gemini mock generated"
}

###############################################################################
# 5. OPENCODE — ~/.config/opencode/ + ~/.local/share/opencode/
###############################################################################
generate_opencode() {
    local config_base="$OUTPUT_DIR/config_opencode"
    local data_base="$OUTPUT_DIR/local_share_opencode"

    mkdir -p "$config_base"
    mkdir -p "$data_base"/{storage,log,tool-output}

    # ~/.local/share/opencode/opencode.db (mock SQLite database)
    python3 -c "
import sqlite3
db_path = '$data_base/opencode.db'
conn = sqlite3.connect(db_path)
conn.execute('CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, project TEXT, start_time TEXT, end_time TEXT, message_count INTEGER)')
conn.execute('CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT, updated_at TEXT)')
conn.execute(\"INSERT INTO sessions (id, project, start_time, end_time, message_count) VALUES ('[REDACTED_SESSION_ID]', '/data/projects/coding_agent_settings_backup', '2026-05-20T10:30:00Z', '2026-05-20T11:45:00Z', 45)\")
conn.execute(\"INSERT INTO settings (key, value, updated_at) VALUES ('theme', 'dark', '2026-05-20T00:00:00Z')\")
conn.execute(\"INSERT INTO settings (key, value, updated_at) VALUES ('model', '[REDACTED_MODEL_NAME]', '2026-05-20T00:00:00Z')\")
conn.commit()
conn.close()
" 2>/dev/null || touch "$data_base/opencode.db"

    # ~/.config/opencode/opencode.json
    cat > "$config_base/opencode.json" << 'EOF'
{
  "theme": "dark",
  "model": "[REDACTED_MODEL_NAME]",
  "provider": "openai",
  "apiKey": "[REDACTED]",
  "mcpServers": {
    "frankensearch": {
      "command": "npx",
      "args": ["-y", "@anthropic-ai/mcp-server-frankensearch"]
    }
  },
  "features": {
    "agent_mail": true,
    "beads": true,
    "skills": true
  }
}
EOF

    # ~/.config/opencode/oh-my-openagent.json
    cat > "$config_base/oh-my-openagent.json" << 'EOF'
{
  "version": "1.0.0",
  "agents": {
    "default": {
      "model": "[REDACTED_MODEL_NAME]",
      "provider": "openai",
      "max_tokens": 8192
    }
  },
  "orchestration": {
    "max_parallel_agents": 5,
    "timeout_seconds": 1800
  }
}
EOF

    # ~/.config/opencode/dcp.jsonc
    cat > "$config_base/dcp.jsonc" << 'EOF'
{
  // Destructive Command Guard configuration
  "enabled": true,
  "packs": ["git", "filesystem", "database"],
  "allow_once_codes": [],
  "blocked_patterns": [
    "rm -rf /",
    "git reset --hard",
    "git clean -fd",
    "DROP DATABASE",
    "kubectl delete"
  ]
}
EOF

    # ~/.local/share/opencode/storage/
    cat > "$data_base/storage/session-cache.json" << 'EOF'
{
  "sessions": [
    {
      "id": "[REDACTED_SESSION_ID]",
      "project": "/data/projects/coding_agent_settings_backup",
      "startTime": "2026-05-20T10:30:00Z",
      "messageCount": 45
    }
  ]
}
EOF

    # ~/.local/share/opencode/log/
    cat > "$data_base/log/opencode.log" << 'EOF'
[2026-05-20T10:30:00Z] INFO  Starting OpenCode session
[2026-05-20T10:30:01Z] INFO  Loading config from ~/.config/opencode/opencode.json
[2026-05-20T10:30:02Z] INFO  Model: [REDACTED_MODEL_NAME]
[2026-05-20T10:30:03Z] INFO  MCP servers initialized: frankensearch
[2026-05-20T10:30:15Z] INFO  User message: "Port asb to Rust"
[2026-05-20T10:31:00Z] INFO  Tool call: bash (ls -la /tmp/...)
[2026-05-20T11:45:00Z] INFO  Session ended, tokens: input=125000 output=35000
EOF

    # ~/.local/share/opencode/tool-output/
    cat > "$data_base/tool-output/last-explorer-result.json" << 'EOF'
{
  "tool": "explore",
  "task_id": "bg_84091bba",
  "status": "completed",
  "result_summary": "Found 6 installed agents on system"
}
EOF

    echo "  ✓ OpenCode mock generated"
}

###############################################################################
# 6. KIRO — ~/.kiro/
###############################################################################
generate_kiro() {
    local base="$OUTPUT_DIR/.kiro"
    mkdir -p "$base"/{settings,"sessions/cli","skills/codebase-audit","skills/testing-conformance-harnesses",agents}

    # ~/.kiro/settings/cli.json
    cat > "$base/settings/cli.json" << 'EOF'
{
  "model": "[REDACTED_MODEL_NAME]",
  "provider": "anthropic",
  "apiKey": "[REDACTED]",
  "maxTokens": 8192,
  "temperature": 0.7,
  "systemPrompt": "You are a helpful coding assistant.",
  "features": {
    "skills": true,
    "agents": true,
    "sessions": true
  }
}
EOF

    # ~/.kiro/settings/feed_state.json
    cat > "$base/settings/feed_state.json" << 'EOF'
{
  "lastFeedUpdate": "2026-05-20T00:00:00Z",
  "seenFeatures": ["skills", "agents", "sessions"],
  "dismissedTips": ["tip-001", "tip-002"],
  "usageStats": {
    "totalSessions": 156,
    "totalMessages": 4523,
    "favoriteModel": "[REDACTED_MODEL_NAME]"
  }
}
EOF

    # ~/.kiro/sessions/cli/ — sample sessions
    for i in 1 2 3; do
        cat > "$base/sessions/cli/session-2026052${i}-001.json" << EOF
{
  "sessionId": "[REDACTED_SESSION_ID_${i}]",
  "startTime": "2026-05-2${i}T10:00:00Z",
  "endTime": "2026-05-2${i}T11:30:00Z",
  "model": "[REDACTED_MODEL_NAME]",
  "messageCount": $((20 + i * 5)),
  "project": "/data/projects/project-alpha"
}
EOF
        cat > "$base/sessions/cli/session-2026052${i}-001.jsonl" << EOF
{"role":"user","content":"Help me understand this codebase","timestamp":"2026-05-2${i}T10:00:00Z"}
{"role":"assistant","content":"Let me explore the project structure...","timestamp":"2026-05-2${i}T10:00:15Z"}
{"role":"tool","name":"explore","content":"Found src/, tests/, Cargo.toml","timestamp":"2026-05-2${i}T10:01:00Z"}
EOF
    done

    # ~/.kiro/skills/
    for skill in codebase-audit testing-conformance-harnesses; do
        mkdir -p "$base/skills/$skill"
        cat > "$base/skills/$skill/SKILL.md" << EOF
# $skill

Auto-generated mock skill for development testing.

## Description
Mock skill file for testing the coding_agent_settings_backup tool.

## Trigger
'test trigger for $skill'
EOF
    done

    # ~/.kiro/agents/
    cat > "$base/agents/agent-config.json" << 'EOF'
{
  "agents": [
    {
      "name": "default",
      "model": "[REDACTED_MODEL_NAME]",
      "provider": "anthropic",
      "maxTokens": 8192
    }
  ]
}
EOF

    # ~/.kiro/.cli_bash_history
    cat > "$base/.cli_bash_history" << 'EOF'
cargo init --name coding_agent_settings_backup
ls -la ~/.kiro/
kiro --help
kiro session list
kiro skill list
EOF

    echo "  ✓ Kiro mock generated"
}

###############################################################################
# MAIN
###############################################################################

echo ""
echo "=== Generating Mock Agent Configs ==="
echo ""

if should_generate "claude"; then
    generate_claude
fi

if should_generate "codex"; then
    generate_codex
fi

if should_generate "cursor"; then
    generate_cursor
fi

if should_generate "gemini"; then
    generate_gemini
fi

if should_generate "opencode"; then
    generate_opencode
fi

if should_generate "kiro"; then
    generate_kiro
fi

echo ""
echo "=== Mock generation complete ==="
echo "Output directory: $OUTPUT_DIR"
echo ""
echo "Structure:"
find "$OUTPUT_DIR" -type f | head -50
echo "..."
echo ""
echo "Total files: $(find "$OUTPUT_DIR" -type f | wc -l)"
echo "Total size: $(du -sh "$OUTPUT_DIR" | cut -f1)"
