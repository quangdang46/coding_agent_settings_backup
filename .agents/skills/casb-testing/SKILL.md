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
casb config get general.backup_root
casb config get backup.max_file_size_mb
```

### 16. completion
```bash
casb completion bash > /tmp/casb-completion.bash
casb completion zsh > /tmp/casb-completion.zsh
casb completion fish > /tmp/casb-completion.fish
```

---

## What To Look For

The commands above are only half the test. These are the behaviours that
have actually broken, and how to spot them:

| Check | Expected | Symptom of a bug |
|-------|----------|------------------|
| Backup repo size | A few MB | Hundreds of MB → a large binary (`.app` bundle, downloaded CLI version) was committed |
| `du -sh $CASB_BACKUP_ROOT/.git/objects/pack` | One `.pack` file | Many packs → git's automatic gc duplicated a blob per commit |
| `ls $CASB_BACKUP_ROOT/.claude` | `home/ data/ root/` present | A multi-location agent's directory was pruned away |
| `casb diff claude` | A handful of entries | Thousands of "added" files → the exclusion filter is not being applied to the diff |
| `casb history claude` | Only claude's commits | Every agent's commits → the log is not scoped to the agent |
| `casb stats` | `total: commits=N` equals the repo's commit count | A multiple of N → the shared repo's count is being summed per agent |
| `casb list` | Only backed-up agents show a commit hash | Every agent shows one → the status is repo-global, not per agent |
| `casb backup --parallel` | Exit 0, one commit per agent | `index.lock` errors → git operations ran concurrently on one index |
| `casb restore <agent>` | Excluded paths survive | `projects/`, `history.jsonl` deleted → the restore used a different filter than the backup |

### The restore check matters most

A restore is the only command that writes to the live agent directories.
Always confirm that the paths the backup excludes are still there after it:

```bash
export CASB_BACKUP_ROOT=/tmp/casb-test
ls ~/.claude/projects > /tmp/before.txt
casb restore claude --force
ls ~/.claude/projects > /tmp/after.txt
diff /tmp/before.txt /tmp/after.txt && echo "excluded paths intact"
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
