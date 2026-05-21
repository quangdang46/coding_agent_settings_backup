#!/usr/bin/env bash
# install.sh — One-liner installer for casb (coding_agent_settings_backup).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/quangdang46/coding_agent_settings_backup/main/install.sh | bash
#   curl -fsSL .../install.sh | CASB_PREFIX=$HOME/.local bash
#
# Behaviour:
#   - Builds casb from source via `cargo install --git`.
#   - Falls back to `cargo install --path` if invoked from inside a clone.
#   - Honours CASB_PREFIX (defaults to $HOME/.cargo) and CASB_REF (defaults to main).
#   - Aborts cleanly if cargo or git are missing.

set -euo pipefail

REPO_URL="${CASB_REPO:-https://github.com/quangdang46/coding_agent_settings_backup}"
REF="${CASB_REF:-main}"
PREFIX="${CASB_PREFIX:-$HOME/.cargo}"

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m==>\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m==> ERROR:\033[0m %s\n' "$*" >&2; exit 1; }

require() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

main() {
    require cargo
    require git

    local rust_version
    rust_version="$(rustc --version | awk '{print $2}')"
    log "rust $rust_version detected"

    log "installing casb from $REPO_URL ($REF) into $PREFIX/bin"
    if [[ -f "Cargo.toml" ]] && grep -q 'name = "coding_agent_settings_backup"' Cargo.toml 2>/dev/null; then
        log "detected local clone — installing via --path ."
        CARGO_INSTALL_ROOT="$PREFIX" cargo install --path . --locked --force
    else
        CARGO_INSTALL_ROOT="$PREFIX" cargo install \
            --git "$REPO_URL" \
            --branch "$REF" \
            --locked \
            --force \
            coding_agent_settings_backup
    fi

    local bin="$PREFIX/bin/casb"
    if [[ ! -x "$bin" ]]; then
        die "installation finished but $bin is not executable"
    fi
    log "installed: $bin"

    if ! command -v casb >/dev/null 2>&1; then
        warn "casb is not on PATH; add this to your shell rc:"
        warn "    export PATH=\"$PREFIX/bin:\$PATH\""
    fi

    log "running casb version"
    "$bin" version

    cat <<'NEXT'

Next steps:
  1. casb init                # create the backup root
  2. casb list                # see installed agents
  3. casb backup              # back up everything that's installed
  4. casb doctor              # verify the install

Configuration lives at ~/.config/casb/config.toml (run `casb config init`).
NEXT
}

main "$@"
