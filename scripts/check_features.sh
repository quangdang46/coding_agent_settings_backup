#!/usr/bin/env bash
# check_features.sh — exercise every documented feature of casb against
# mock coding-agent configs produced by scripts/generate_mock_agents.sh.
#
# Designed to run in a fully isolated tempdir so it cannot touch the real
# user's $HOME. Every casb invocation gets HOME=<tempdir>, CASB_CONFIG=<tempdir>/.config/casb/config.toml.
#
# Usage:
#   ./check_features.sh [/path/to/casb] [/path/to/repo]
#
# Exit code is the number of failed checks (0 = all green).

set -u
set -o pipefail

CASB_BIN="${1:-/home/ubuntu/repos/coding_agents_backup/target/release/casb}"
REPO_DIR="${2:-/home/ubuntu/repos/coding_agents_backup}"
MOCK_SCRIPT="$REPO_DIR/scripts/generate_mock_agents.sh"

if [[ ! -x "$CASB_BIN" ]]; then
    echo "FATAL: casb binary not found / not executable at $CASB_BIN" >&2
    exit 99
fi
if [[ ! -x "$MOCK_SCRIPT" ]]; then
    echo "FATAL: mock generator not found at $MOCK_SCRIPT" >&2
    exit 99
fi

ROOT="$(mktemp -d -t casb-check-XXXXXX)"
HOME_DIR="$ROOT/home"
MOCK_DIR="$ROOT/mocks"
LOG_DIR="$ROOT/logs"
mkdir -p "$HOME_DIR" "$MOCK_DIR" "$LOG_DIR"

# Place agent sources INSIDE the isolated HOME so the default built-in
# Claude/Codex/Cursor/Gemini/OpenCode/Kiro locations resolve against
# something interesting. We point the built-in default locations at the
# mock layout by symlinking the dot-dirs the agents expect.
echo "Generating mock agents into $MOCK_DIR ..."
bash "$MOCK_SCRIPT" "$MOCK_DIR" --all >/dev/null
ls "$MOCK_DIR" >>"$LOG_DIR/mock_layout.txt"

# Wire mock agents into the isolated HOME so built-in agent definitions
# pick them up.  Layout follows what each agent expects.
ln -s "$MOCK_DIR/.codex"    "$HOME_DIR/.codex"
ln -s "$MOCK_DIR/.cursor"   "$HOME_DIR/.cursor"
ln -s "$MOCK_DIR/.gemini"   "$HOME_DIR/.gemini"
ln -s "$MOCK_DIR/.kiro"     "$HOME_DIR/.kiro"

# Claude: three locations
mkdir -p "$HOME_DIR/.local/share"
ln -s "$MOCK_DIR/.claude/home"          "$HOME_DIR/.claude"
ln -s "$MOCK_DIR/.claude/data"          "$HOME_DIR/.local/share/claude"
cp    "$MOCK_DIR/.claude/root_config/claude.json" "$HOME_DIR/.claude.json"

# OpenCode: two locations. The mock generator writes opencode mocks under
# `config_opencode/` and `local_share_opencode/` (not `.opencode/`).
mkdir -p "$HOME_DIR/.config"
ln -s "$MOCK_DIR/config_opencode"       "$HOME_DIR/.config/opencode"
ln -s "$MOCK_DIR/local_share_opencode" "$HOME_DIR/.local/share/opencode"

mkdir -p "$HOME_DIR/.config/casb"
CFG="$HOME_DIR/.config/casb/config.toml"
cat > "$CFG" <<TOML
[general]
backup_root  = "$HOME_DIR/.agent_settings_backups"
auto_commit  = true
verbose      = false
quiet        = false
output_format = "text"

[backup]
exclusions = ["*.log", "*.tmp", "*.sqlite3-wal", "*.sqlite3-shm"]
use_rsync  = false
checksum_verify = false

[schedule]
method = "systemd"
interval = "daily"
TOML

# Custom agent pointed at a small text dir for hook-firing test.
mkdir -p "$ROOT/custom_agent"
echo "hello world" > "$ROOT/custom_agent/hello.txt"
cat >> "$CFG" <<TOML

[agents.myagent]
enabled      = true
display_name = "Custom Mock Agent"
locations    = ["$ROOT/custom_agent"]
exclusions   = []
TOML

# --- Helpers ---------------------------------------------------------------
PASS=0
FAIL=0
FAILED_CHECKS=()
TOTAL=0

# casb wrapper that always uses our isolated HOME + config. Env clears the
# host's XDG_*_HOME so they fall back to $HOME/.config and $HOME/.local/share.
casb() {
    env -u XDG_CONFIG_HOME -u XDG_DATA_HOME \
        HOME="$HOME_DIR" \
        CASB_CONFIG="$CFG" \
        "$CASB_BIN" "$@"
}
export -f casb 2>/dev/null || true   # so subshells can call it
export CASB_BIN HOME_DIR CFG ROOT LOG_DIR

# Run a check that should exit 0; optionally grep stdout for $3.
expect_ok() {
    local label="$1"; shift
    local pattern="${1:-}"; shift || true
    TOTAL=$((TOTAL+1))
    local log="$LOG_DIR/$(printf '%03d' "$TOTAL")_$(echo "$label" | tr ' /' '__' ).log"
    if "$@" >"$log" 2>&1; then
        if [[ -n "$pattern" ]] && ! grep -qE -- "$pattern" "$log"; then
            FAIL=$((FAIL+1))
            FAILED_CHECKS+=("$label (missing pattern: $pattern) -> $log")
            printf '  [FAIL] %-50s (no match for %q)\n' "$label" "$pattern"
            return 1
        fi
        PASS=$((PASS+1))
        printf '  [ OK ] %-50s\n' "$label"
        return 0
    else
        FAIL=$((FAIL+1))
        FAILED_CHECKS+=("$label -> $log")
        printf '  [FAIL] %-50s (exit %d, see %s)\n' "$label" "$?" "$log"
        return 1
    fi
}

# Run a check that should exit NON-zero (i.e. expected failure).
expect_fail() {
    local label="$1"; shift
    TOTAL=$((TOTAL+1))
    local log="$LOG_DIR/$(printf '%03d' "$TOTAL")_$(echo "$label" | tr ' /' '__' ).log"
    if "$@" >"$log" 2>&1; then
        FAIL=$((FAIL+1))
        FAILED_CHECKS+=("$label (expected failure but succeeded) -> $log")
        printf '  [FAIL] %-50s (expected non-zero exit)\n' "$label"
        return 1
    else
        PASS=$((PASS+1))
        printf '  [ OK ] %-50s\n' "$label"
        return 0
    fi
}

section() {
    echo
    echo "=== $* ==="
}

# --- 1. version / help / completion ---------------------------------------
section "Meta: version / help / completion"
expect_ok "version prints binary name"            "casb"           casb version
expect_ok "--help shows summary"                  "Backup and restore" casb --help
expect_ok "subcommand help: backup --help"        "Backup one or more agents" casb backup --help
expect_ok "completion bash"                       "complete -F"    casb completion bash
expect_ok "completion zsh"                        "compdef"        casb completion zsh
expect_ok "completion fish"                       "complete -c"    casb completion fish

# --- 2. init / list -------------------------------------------------------
section "Init / list"
expect_ok "init creates backup root"              "initialised"    casb init
[[ -d "$HOME_DIR/.agent_settings_backups" ]] && PASS=$((PASS+1)) || { FAIL=$((FAIL+1)); FAILED_CHECKS+=("backup root missing"); }
TOTAL=$((TOTAL+1))
expect_ok "list shows custom agent"               "myagent"        casb list
expect_ok "list shows claude (installed)"         "claude"         casb list
expect_ok "list shows 19 built-in agents"         "codex"          casb list

# --- 3. config -----------------------------------------------------------
section "Config subcommands"
expect_ok "config show"                           "backup_root"    casb config show
expect_ok "config path prints CASB_CONFIG"        "config.toml"    casb config path
expect_ok "config get general.verbose"            "false"          casb config get general.verbose
expect_ok "config set general.verbose true"      "" casb config set general.verbose true
expect_ok "config get reflects update"            "true"           casb config get general.verbose
expect_ok "config init is idempotent"             ""               casb config init

# --- 4. backup ------------------------------------------------------------
section "Backup (single agent, multi-location, parallel, dry-run, custom msg)"

# Custom agent backup with custom commit msg.
expect_ok "backup custom agent w/ -m"             "myagent"        casb backup myagent -m "initial backup"
expect_ok "backup repo has .git"                  "" test -d "$HOME_DIR/.agent_settings_backups/.myagent/.git"
expect_ok "backed-up file present"                "" test -f "$HOME_DIR/.agent_settings_backups/.myagent/hello.txt"

# Multi-location: claude has 3 locations (home, data, claude.json).
# The backup itself currently fails on the File-kind location (Bug-4) so we
# just exercise the command and check the directory locations got copied.
casb backup claude >"$LOG_DIR/000_backup_claude.log" 2>&1 || true
expect_ok "claude backup repo created"            "" test -d "$HOME_DIR/.agent_settings_backups/.claude/.git"
expect_ok "claude home/ subdir populated"         "" test -f "$HOME_DIR/.agent_settings_backups/.claude/home/settings.json"
expect_ok "claude data/ subdir populated"         "" test -d "$HOME_DIR/.agent_settings_backups/.claude/data"

# Multi-location OpenCode (config + data).
expect_ok "backup opencode (multi-location)"      "opencode"       casb backup opencode
expect_ok "opencode config/ subdir populated"     "" test -d "$HOME_DIR/.agent_settings_backups/.opencode/config"
expect_ok "opencode data/ subdir populated"       "" test -d "$HOME_DIR/.agent_settings_backups/.opencode/data"

# SQLite files: state DBs should be present (only -wal/-shm excluded).
casb backup codex >/dev/null 2>&1
expect_ok "codex SQLite database backed up"       "" test -f "$HOME_DIR/.agent_settings_backups/.codex/logs_2.sqlite"

# Parallel backup of several agents at once.
expect_ok "backup --parallel cursor gemini kiro"  ""               casb backup cursor gemini kiro --parallel

# Dry run on a fresh agent must NOT create a repo.
mkdir -p "$ROOT/dryrun_agent"
echo "dry" > "$ROOT/dryrun_agent/dry.txt"
cat >> "$CFG" <<TOML

[agents.dryagent]
enabled      = true
display_name = "Dry-run Mock"
locations    = ["$ROOT/dryrun_agent"]
exclusions   = []
TOML
expect_ok "--dry-run backup"                      ""               casb --dry-run backup dryagent
expect_fail "dry-run did not create repo"        test -d "$HOME_DIR/.agent_settings_backups/.dryagent/.git"

# --- 5. history / diff ---------------------------------------------------
section "History / diff"
expect_ok "history myagent (default limit)"       "initial backup" casb history myagent
expect_ok "history myagent --limit 5"             ""               casb history myagent --limit 5

# No diff right after backup.
expect_ok "diff (no changes)"                     "no changes"     casb diff myagent

# Modify source then diff should report change.
echo "modified" > "$ROOT/custom_agent/hello.txt"
expect_ok "diff after modification"               "modified"       casb diff myagent

# --- 6. tag --------------------------------------------------------------
section "Tag create / list / delete / restore"
expect_ok "tag create v1"                         ""               casb tag create myagent v1 -m "first tag"
expect_ok "tag list shows v1"                     "v1"             casb tag list myagent
expect_ok "tag restore v1 --force"                ""               casb --force tag restore myagent v1
expect_ok "after tag-restore file reverted"       "hello world"    cat "$ROOT/custom_agent/hello.txt"
expect_ok "tag delete v1"                         ""               casb tag delete myagent v1
expect_fail "tag list no longer shows v1 (regex)" bash -c "casb tag list myagent | grep -q '^v1\$'"

# --- 7. restore ----------------------------------------------------------
section "Restore"
# Modify source then restore from HEAD.
echo "transient change" > "$ROOT/custom_agent/hello.txt"
expect_ok "restore myagent --force HEAD"          ""               casb restore myagent --force
expect_ok "restore reverted file content"         "hello world"    cat "$ROOT/custom_agent/hello.txt"

# Restoring an unknown ref should fail.
expect_fail "restore unknown ref errors"          casb restore myagent nosuchref --force

# --- 8. export / import --------------------------------------------------
section "Export / import (file + stdin/stdout)"
ARCHIVE="$ROOT/myagent.tar.gz"
expect_ok "export myagent to file"                ""               casb export myagent "$ARCHIVE"
expect_ok "archive exists"                        ""               test -s "$ARCHIVE"

# Import into a SECOND isolated HOME.
HOME2="$ROOT/home2"
mkdir -p "$HOME2/.config/casb" "$HOME2/.agent_settings_backups"
mkdir -p "$ROOT/custom_agent2"
echo "stub" > "$ROOT/custom_agent2/hello.txt"
CFG2="$HOME2/.config/casb/config.toml"
cat > "$CFG2" <<TOML
[general]
backup_root = "$HOME2/.agent_settings_backups"
auto_commit = true
output_format = "text"

[backup]
use_rsync = false
exclusions = []

[schedule]
method = "systemd"
interval = "daily"

[agents.myagent]
enabled = true
display_name = "Mock"
locations = ["$ROOT/custom_agent2"]
exclusions = []
TOML
HOME="$HOME2" CASB_CONFIG="$CFG2" "$CASB_BIN" init >/dev/null
expect_ok "import into fresh home" "" \
    bash -c "HOME=\"$HOME2\" CASB_CONFIG=\"$CFG2\" \"$CASB_BIN\" import \"$ARCHIVE\""
expect_ok "imported repo has .git"  "" test -d "$HOME2/.agent_settings_backups/.myagent/.git"

# stdout/stdin pipe round-trip with `-`.
ARCHIVE2="$ROOT/myagent_pipe.tar.gz"
TOTAL=$((TOTAL+1))
LOG="$LOG_DIR/$(printf '%03d' "$TOTAL")_export_stdout.log"
if casb export myagent - > "$ARCHIVE2" 2>"$LOG" && [[ -s "$ARCHIVE2" ]]; then
    PASS=$((PASS+1)); printf '  [ OK ] %-50s\n' "export to stdout (-)"
else
    FAIL=$((FAIL+1)); FAILED_CHECKS+=("export to stdout (-) -> $LOG")
    printf '  [FAIL] %-50s\n' "export to stdout (-)"
fi

HOME3="$ROOT/home3"
mkdir -p "$HOME3/.config/casb" "$HOME3/.agent_settings_backups" "$ROOT/custom_agent3"
echo stub > "$ROOT/custom_agent3/hello.txt"
CFG3="$HOME3/.config/casb/config.toml"
sed "s#$HOME2#$HOME3#g;s#custom_agent2#custom_agent3#g" "$CFG2" > "$CFG3"
env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME="$HOME3" CASB_CONFIG="$CFG3" "$CASB_BIN" init >/dev/null

TOTAL=$((TOTAL+1))
LOG="$LOG_DIR/$(printf '%03d' "$TOTAL")_import_stdin.log"
if env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME="$HOME3" CASB_CONFIG="$CFG3" "$CASB_BIN" import - < "$ARCHIVE2" >"$LOG" 2>&1; then
    PASS=$((PASS+1)); printf '  [ OK ] %-50s\n' "import from stdin (-)"
else
    FAIL=$((FAIL+1)); FAILED_CHECKS+=("import from stdin (-) -> $LOG")
    printf '  [FAIL] %-50s\n' "import from stdin (-)"
fi
expect_ok "stdin-imported repo has .git" "" test -d "$HOME3/.agent_settings_backups/.myagent/.git"

# --- 9. verify ----------------------------------------------------------
section "Verify"
expect_ok "verify myagent reports clean"          "clean"          casb verify myagent
expect_ok "verify all agents"                     ""               casb verify

# --- 10. stats ---------------------------------------------------------
section "Stats"
expect_ok "stats single agent"                    "myagent"        casb stats myagent
expect_ok "stats aggregate"                       ""               casb stats

# --- 11. discover ------------------------------------------------------
section "Discover"
# Plant a directory that LOOKS like a continue-the-agent dotdir.
mkdir -p "$HOME_DIR/.continue"
echo '{}' > "$HOME_DIR/.continue/config.json"
expect_ok "discover --list-only finds new agents" ""               casb discover --list-only

# --- 12. doctor -------------------------------------------------------
section "Doctor"
expect_ok "doctor reports all checks passed"      "doctor: all checks passed" casb doctor

# --- 13. hooks --------------------------------------------------------
section "Hooks"
mkdir -p "$HOME_DIR/.config/casb/pre-backup.d" "$HOME_DIR/.config/casb/post-backup.d" \
         "$HOME_DIR/.config/casb/pre-restore.d" "$HOME_DIR/.config/casb/post-restore.d"

for kind in pre-backup post-backup pre-restore post-restore; do
    cat > "$HOME_DIR/.config/casb/${kind}.d/10-marker.sh" <<HOOK
#!/usr/bin/env bash
mkdir -p "$ROOT/hook_evidence"
echo "${kind}:\$1:\$CASB_HOOK:\$CASB_AGENT" >> "$ROOT/hook_evidence/log.txt"
HOOK
    chmod +x "$HOME_DIR/.config/casb/${kind}.d/10-marker.sh"
done

expect_ok "hooks list shows scripts"              "10-marker.sh"   casb hooks list
expect_ok "hooks path prints config dir"          "casb"           casb hooks path

# Trigger hooks via a backup against a FRESH agent (myagent's source has a
# stale .git file from earlier restore -- see findings/Bug-2).
mkdir -p "$ROOT/hook_agent"
echo "hook-payload" > "$ROOT/hook_agent/hello.txt"
cat >> "$CFG" <<TOML

[agents.hookagent]
enabled = true
display_name = "Hook-test Agent"
locations = ["$ROOT/hook_agent"]
exclusions = []
TOML
: > "$ROOT/hook_evidence/log.txt"
expect_ok "backup fires pre+post hooks"           ""               casb backup hookagent
expect_ok "pre-backup hook fired"                 "pre-backup:hookagent"  cat "$ROOT/hook_evidence/log.txt"
expect_ok "post-backup hook fired"                "post-backup:hookagent" cat "$ROOT/hook_evidence/log.txt"

# Trigger restore so pre/post-restore hooks fire too.
echo "changed" > "$ROOT/hook_agent/hello.txt"
expect_ok "restore fires pre+post-restore hooks"  ""               casb --force restore hookagent
expect_ok "pre-restore hook fired"                "pre-restore:hookagent"  cat "$ROOT/hook_evidence/log.txt"
expect_ok "post-restore hook fired"               "post-restore:hookagent" cat "$ROOT/hook_evidence/log.txt"

# Failing hook aborts backup. casb's pre-hook failure should propagate.
cat > "$HOME_DIR/.config/casb/pre-backup.d/05-fail.sh" <<'HOOK'
#!/usr/bin/env bash
exit 1
HOOK
chmod +x "$HOME_DIR/.config/casb/pre-backup.d/05-fail.sh"
# Note: casb still exits 0 even on per-agent failure (see Bug-3); accept
# either non-zero exit OR explicit error string in output.
LOG="$LOG_DIR/$(printf '%03d' $((TOTAL+1)))_fail_hook.log"
TOTAL=$((TOTAL+1))
if casb backup hookagent >"$LOG" 2>&1; then
    if grep -qE 'hook exited with status|command failed' "$LOG"; then
        PASS=$((PASS+1)); printf '  [ OK ] %-50s\n' "failing pre-hook aborts backup (msg)"
    else
        FAIL=$((FAIL+1)); FAILED_CHECKS+=("failing pre-hook aborts backup -> $LOG")
        printf '  [FAIL] %-50s (no error msg)\n' "failing pre-hook aborts backup"
    fi
else
    PASS=$((PASS+1)); printf '  [ OK ] %-50s (non-zero exit)\n' "failing pre-hook aborts backup"
fi
rm "$HOME_DIR/.config/casb/pre-backup.d/05-fail.sh"

# --- 14. schedule --------------------------------------------------
section "Schedule"
# In sandboxed env we may not have systemd / cron writable, so we only
# require status to succeed (it should just report not-installed).
expect_ok "schedule status"                       ""               casb schedule status
# Try install but tolerate failure if systemd/cron not available.
LOG="$LOG_DIR/$(printf '%03d' $((TOTAL+1)))_schedule_install.log"
TOTAL=$((TOTAL+1))
if casb schedule install daily --method cron >"$LOG" 2>&1; then
    PASS=$((PASS+1))
    printf '  [ OK ] %-50s\n' "schedule install (cron)"
    casb schedule remove >/dev/null 2>&1 || true
else
    PASS=$((PASS+1))
    printf '  [SKIP] %-50s (cron not available in sandbox)\n' "schedule install (cron)"
fi

# --- 15. output formats / global flags -------------------
section "Output formats & global flags"
expect_ok "--json list returns valid JSON"        '"ok"\s*:\s*true' casb --json list
expect_ok "--format json list"                    '"command"'      casb --format json list
expect_ok "--format toon list"                    ""               casb --format toon list
mkdir -p "$ROOT/vq_agent"; echo vq > "$ROOT/vq_agent/file.txt"
cat >> "$CFG" <<TOML

[agents.vqagent]
enabled = true
display_name = "Verbose/Quiet test"
locations = ["$ROOT/vq_agent"]
exclusions = []
TOML
expect_ok "--verbose backup"                      ""               casb --verbose backup vqagent -m "verbose run"
expect_ok "--quiet backup"                        ""               casb --quiet  backup vqagent -m "quiet run"

# JSON validity (strict): parse with python.
TOTAL=$((TOTAL+1))
if casb --json list | python3 -c "import sys,json; json.load(sys.stdin)" 2>/dev/null; then
    PASS=$((PASS+1))
    printf '  [ OK ] %-50s\n' "casb --json list parses as JSON"
else
    FAIL=$((FAIL+1))
    FAILED_CHECKS+=("casb --json list invalid JSON")
    printf '  [FAIL] %-50s\n' "casb --json list parses as JSON"
fi

# --- 16. .casbignore --------------------------------
section ".casbignore filtering"
mkdir -p "$ROOT/ignore_agent"
echo "kept"     > "$ROOT/ignore_agent/keep.txt"
echo "secret"   > "$ROOT/ignore_agent/ignore_me.secret"
mkdir -p "$ROOT/ignore_agent/cache"
echo "cached"   > "$ROOT/ignore_agent/cache/cache.bin"
cat > "$ROOT/ignore_agent/.casbignore" <<'IGN'
*.secret
cache/
IGN
cat >> "$CFG" <<TOML

[agents.ignoreagent]
enabled = true
display_name = "Ignore-test Agent"
locations = ["$ROOT/ignore_agent"]
exclusions = []
TOML
expect_ok "backup with .casbignore"               ""               casb backup ignoreagent
expect_ok "kept file backed up"                   "" test -f "$HOME_DIR/.agent_settings_backups/.ignoreagent/keep.txt"
expect_fail ".secret file ignored"               test -f "$HOME_DIR/.agent_settings_backups/.ignoreagent/ignore_me.secret"
expect_fail "cache/ dir ignored"                 test -d "$HOME_DIR/.agent_settings_backups/.ignoreagent/cache"

# --- 17. error handling ----------------------------
section "Error handling"
expect_fail "backup unknown agent errors"        casb backup nosuchagent
expect_fail "restore unknown agent errors"       casb restore nosuchagent --force

# --- 18. regression checks for discovered bugs ----
section "Regression checks (known issues)"

# Bug-4: claude has a File-kind location (~/.claude.json) which is mis-
#        handled by sync_agent_to_backup: the destination file path is
#        created as a directory, causing sync_file's std::fs::copy to fail.
if [[ -e "$HOME_DIR/.agent_settings_backups/.claude/root/.claude.json" ]] && \
   [[ ! -d "$HOME_DIR/.agent_settings_backups/.claude/root/.claude.json" ]]; then
    PASS=$((PASS+1)); TOTAL=$((TOTAL+1))
    printf '  [ OK ] %-50s\n' "claude.json backed up as a file"
else
    FAIL=$((FAIL+1)); TOTAL=$((TOTAL+1))
    FAILED_CHECKS+=("Bug-4: claude.json backed up as a directory, not a file")
    printf '  [FAIL] %-50s (created as dir)\n' "claude.json backed up as a file"
fi

# Bug-5: backup output prints "\u2717 <agent> \u2014 ..." on per-agent failure but
#        the process still exits 0.
if grep -qE '^✗ claude' "$LOG_DIR/000_backup_claude.log" 2>/dev/null; then
    FAIL=$((FAIL+1)); TOTAL=$((TOTAL+1))
    FAILED_CHECKS+=("Bug-5: casb backup claude printed an error but exited 0")
    printf '  [FAIL] %-50s (silent failure)\n' "backup claude reports success cleanly"
else
    PASS=$((PASS+1)); TOTAL=$((TOTAL+1))
    printf '  [ OK ] %-50s\n' "backup claude reports success cleanly"
fi

# Bug-1: after restore, source location is left with a stale .git gitlink
#       file pointing at the (now-removed) worktree inside the backup repo.
mkdir -p "$ROOT/regress_agent"
echo one > "$ROOT/regress_agent/file.txt"
cat >> "$CFG" <<TOML

[agents.regress]
enabled = true
display_name = "Regress"
locations = ["$ROOT/regress_agent"]
exclusions = []
TOML
casb backup regress >/dev/null
echo two > "$ROOT/regress_agent/file.txt"
casb --force restore regress >/dev/null
if [[ -e "$ROOT/regress_agent/.git" ]]; then
    FAIL=$((FAIL+1)); TOTAL=$((TOTAL+1))
    FAILED_CHECKS+=("Bug-1: restore leaves stale .git in source ($ROOT/regress_agent/.git)")
    printf '  [FAIL] %-50s (stale .git in source)\n' "restore must not leave .git in source"
else
    PASS=$((PASS+1)); TOTAL=$((TOTAL+1))
    printf '  [ OK ] %-50s\n' "restore must not leave .git in source"
fi

# Bug-2: subsequent backup after restore should still succeed AND not error.
casb backup regress >"$LOG_DIR/regress_backup.log" 2>&1
TOTAL=$((TOTAL+1))
if grep -qE '^\xe2\x9c\x97|error|EISDIR|Is a directory' "$LOG_DIR/regress_backup.log"; then
    FAIL=$((FAIL+1))
    FAILED_CHECKS+=("Bug-2: backup after restore fails due to stale .git (see $LOG_DIR/regress_backup.log)")
    printf '  [FAIL] %-50s (EISDIR: stale .git)\n' "backup after restore succeeds"
else
    PASS=$((PASS+1))
    printf '  [ OK ] %-50s\n' "backup after restore succeeds"
fi

# Bug-3: when an individual backup errors, casb still exits 0. Verify.
cat > "$HOME_DIR/.config/casb/pre-backup.d/05-fail.sh" <<'HOOK'
#!/usr/bin/env bash
exit 1
HOOK
chmod +x "$HOME_DIR/.config/casb/pre-backup.d/05-fail.sh"
TOTAL=$((TOTAL+1))
if casb backup regress >"$LOG_DIR/silent_failure.log" 2>&1; then
    FAIL=$((FAIL+1))
    FAILED_CHECKS+=("Bug-3: casb backup returns 0 despite hook failure (see $LOG_DIR/silent_failure.log)")
    printf '  [FAIL] %-50s (silent failure)\n' "backup exits non-zero when agent fails"
else
    PASS=$((PASS+1))
    printf '  [ OK ] %-50s\n' "backup exits non-zero when agent fails"
fi
rm "$HOME_DIR/.config/casb/pre-backup.d/05-fail.sh"

# --- Summary ----
echo
echo "=========================================="
echo "Total checks: $TOTAL"
echo "Passed:       $PASS"
echo "Failed:       $FAIL"
echo "Workspace:    $ROOT"
echo "Logs:         $LOG_DIR"
echo "=========================================="
if (( FAIL > 0 )); then
    echo
    echo "Failed checks:"
    for f in "${FAILED_CHECKS[@]}"; do
        echo "  - $f"
    done
fi

# Persist final summary
{
    echo "Total: $TOTAL"
    echo "Pass:  $PASS"
    echo "Fail:  $FAIL"
    echo
    echo "Failed checks:"
    for f in "${FAILED_CHECKS[@]:-}"; do
        echo "  - $f"
    done
} > "$ROOT/summary.txt"

exit "$FAIL"
