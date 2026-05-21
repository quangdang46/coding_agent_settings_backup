#!/usr/bin/env bash
# release.sh — build a versioned release archive for casb.
#
# Produces target/release-artifacts/casb-<version>-<target>.tar.gz containing:
#   - bin/casb
#   - completions/{bash,zsh,fish}
#   - scripts/{install.sh,install.ps1}
#   - README.md
#   - LICENSE
#
# Optional: pass --tag to git-tag the current HEAD as v<version>.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DO_TAG=false
for arg in "$@"; do
    case "$arg" in
        --tag) DO_TAG=true ;;
        *) echo "unknown arg: $arg" >&2; exit 1 ;;
    esac
done

VERSION="$(awk -F'"' '/^version =/ { print $2; exit }' Cargo.toml)"
TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
NAME="casb-${VERSION}-${TARGET}"
OUT="target/release-artifacts"

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m==>\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m==> ERROR:\033[0m %s\n' "$*" >&2; exit 1; }

log "version $VERSION, target $TARGET"

log "running tests"
cargo test --quiet >/dev/null

log "running clippy"
cargo clippy --all-targets -- -D warnings

log "checking formatting"
cargo fmt --all -- --check

log "building release binary"
cargo build --release --locked

BIN="target/release/casb"
[[ -x "$BIN" ]] || die "release binary not found at $BIN"

log "generating completion scripts"
mkdir -p "$OUT/$NAME"/{bin,completions,scripts}
cp "$BIN" "$OUT/$NAME/bin/casb"
"$BIN" completion bash > "$OUT/$NAME/completions/casb.bash"
"$BIN" completion zsh  > "$OUT/$NAME/completions/_casb"
"$BIN" completion fish > "$OUT/$NAME/completions/casb.fish"
cp scripts/install.sh "$OUT/$NAME/scripts/install.sh"
cp scripts/install.ps1 "$OUT/$NAME/scripts/install.ps1"
cp README.md LICENSE "$OUT/$NAME/"

log "creating archive $OUT/${NAME}.tar.gz"
tar -czf "$OUT/${NAME}.tar.gz" -C "$OUT" "$NAME"

log "computing checksum"
( cd "$OUT" && sha256sum "${NAME}.tar.gz" | tee "${NAME}.tar.gz.sha256" )

log "release artefact summary"
ls -lh "$OUT"

if [[ "$DO_TAG" == "true" ]]; then
    log "git tagging v$VERSION"
    git tag -a "v$VERSION" -m "Release v$VERSION"
    log "tag created. push with: git push origin v$VERSION"
fi

cat <<NEXT

Done. Next steps:
  - Inspect: tar -tzf $OUT/${NAME}.tar.gz | head
  - Test:    tar -xzf $OUT/${NAME}.tar.gz && ./$NAME/bin/casb version
  - Tag:     ./scripts/release.sh --tag
  - Push:    git push origin v$VERSION
NEXT
