---
name: casb-testing
description: >-
  Test casb (Coding Agent Settings Backup) with real agent source folders but a
  temp backup root — no risk to production backups. Covers all commands: init,
  backup, restore, list, history, diff, stats, verify, tag, export, import,
  doctor, discover, schedule, hooks, config, dry-run.
---

# casb Testing

> **Core Capability:** Safely test every `casb` feature using real agent settings
> as source, but redirecting the backup repository to a temporary directory.
> Zero impact on the actual `~/.agent_settings_backups/`.

## Quick Start

```bash
# Set temp backup root (export so every command inherits it)
export CASB_BACKUP_ROOT=/tmp/casb-test

# Initialize the temp repo
casb init

# Backup all installed agents (reads real ~/.claude/, ~/.cursor/, etc.)
casb backup

# See what happened
casb list
```

When done, clean up:
```bash
rm -rf /tmp/casb-test
unset CASB_BACKUP_ROOT
```

---

## THE EXACT PROMPT — Full Test Suite

> Run every casb command against a tmp backup root. Source folders are
> real agent configs; the git repo lives in `/tmp/casb-test/` and can be
> safely deleted.

```bash
export CASB_BACKUP_ROOT=/tmp/casb-test
```

### 1. init
```bash
casb init
casb init --verbose
```
Expected: creates `/tmp/casb-test/.git` and the backup structure.

### 2. backup
```bash
casb backup
casb backup claude
casb backup claude cursor    # specific agents
casb backup --parallel       # concurrent backups
casb backup --dry-run -v     # preview only
casb backup -m "test snapshot"
```
Expected: reads real folders from `~/.claude/`, `~/.cursor/`, etc.; writes
only to `/tmp/casb-test/`.

### 3. list
```bash
casb list
casb list --json
casb list --format toon
```
Expected: shows real detected agents with their backup status.

### 4. history
```bash
casb history claude
casb history claude --json
casb history claude --oneline
casb history claude -n 5
```
Expected: shows commits made to the tmp repo.

### 5. diff
```bash
casb diff claude
```
Expected: shows changes since the last backup.

### 6. stats
```bash
casb stats
casb stats claude
```
Expected: repo size, commit count, source size from the tmp repo.

### 7. verify
```bash
casb verify
casb verify claude
casb verify --verbose
```
Expected: runs `git fsck` on the tmp repo.

### 8. tag
```bash
casb tag create claude v1-test
casb tag list claude
casb tag delete claude v1-test
```

### 9. export / import
```bash
casb export claude /tmp/claude-backup.tar.gz
casb export claude - | gzip -d -c | tar tf -   # preview via stdout
rm -rf /tmp/casb-restore-test
mkdir /tmp/casb-restore-test
export CASB_BACKUP_ROOT=/tmp/casb-restore-test
casb init
casb import /tmp/claude-backup.tar.gz
unset CASB_BACKUP_ROOT
```

### 10. restore
```bash
# Preview first
casb restore claude --dry-run

# Actual restore from tmp repo
export CASB_BACKUP_ROOT=/tmp/casb-test
casb restore claude
casb restore claude HEAD~1
unset CASB_BACKUP_ROOT
```

### 11. doctor
```bash
casb doctor --verbose
```

### 12. discover
```bash
casb discover
```

### 13. schedule
```bash
casb schedule
casb schedule --help
```

### 14. hooks
```bash
casb hooks
```

### 15. config
```bash
casb config
casb config get backup_root
```

### 16. completion
```bash
casb completion bash > /tmp/casb-completion.bash
casb completion zsh > /tmp/casb-completion.zsh
casb completion fish > /tmp/casb-completion.fish
```

---

## Alternate Approaches

### Using `--config` with a temp config file
```bash
cat > /tmp/casb-config.toml << 'EOF'
[general]
backup_root = "/tmp/casb-test-alt"
auto_commit = true
[backup]
use_rsync = true
checksum_verify = false
EOF

casb --config /tmp/casb-config.toml init
casb --config /tmp/casb-config.toml backup
casb --config /tmp/casb-config.toml list
```

### Using `--dry-run` without any env override
Pure preview — reads no real data, writes nothing:
```bash
casb backup --dry-run -v
casb restore claude --dry-run -v
```

---

## Cleanup

```bash
rm -rf /tmp/casb-test /tmp/casb-test-alt /tmp/casb-restore-test /tmp/claude-backup.tar.gz /tmp/casb-config.toml /tmp/casb-completion.*
```
