# install.ps1 — One-liner installer for casb (coding_agent_settings_backup) on Windows.
#
# Usage (pipe-safe — no [CmdletBinding]/param so iex works):
#   irm "https://raw.githubusercontent.com/quangdang46/coding_agent_settings_backup/main/install.ps1" | iex
#
# Environment knobs:
#   $env:CASB_REPO    Source repository URL.    Default: https://github.com/quangdang46/coding_agent_settings_backup
#   $env:CASB_REF     Git ref to install from.  Default: main
#   $env:CASB_PREFIX  Cargo install root.       Default: $env:USERPROFILE\.cargo
#
# Requires `cargo` (Rust toolchain) and `git` on PATH.

& {
    $ErrorActionPreference = 'Stop'

    function Write-Step([string]$Message) {
        Write-Host "==> $Message" -ForegroundColor Green
    }

    function Write-Warn([string]$Message) {
        Write-Host "==> $Message" -ForegroundColor Yellow
    }

    function Fail([string]$Message) {
        Write-Host "==> ERROR: $Message" -ForegroundColor Red
        throw $Message
    }

    function Require-Command([string]$Name) {
        if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
            Fail "missing required command: $Name"
        }
    }

    $repo   = if ($env:CASB_REPO)   { $env:CASB_REPO }   else { 'https://github.com/quangdang46/coding_agent_settings_backup' }
    $ref    = if ($env:CASB_REF)    { $env:CASB_REF }    else { 'main' }
    $prefix = if ($env:CASB_PREFIX) { $env:CASB_PREFIX } else { Join-Path $env:USERPROFILE '.cargo' }

    Require-Command 'cargo'
    Require-Command 'git'

    $rustVersion = (& rustc --version) -split ' ' | Select-Object -Index 1
    Write-Step "rust $rustVersion detected"

    Write-Step "installing casb from $repo ($ref) into $prefix\bin"

    $env:CARGO_INSTALL_ROOT = $prefix
    $cwdHasManifest = Test-Path 'Cargo.toml'
    if ($cwdHasManifest) {
        $manifest = Get-Content 'Cargo.toml' -Raw
        if ($manifest -match 'name = "coding_agent_settings_backup"') {
            Write-Step 'detected local clone — installing via --path .'
            & cargo install --path . --locked --force
            if ($LASTEXITCODE -ne 0) { Fail 'cargo install failed' }
        } else {
            $cwdHasManifest = $false
        }
    }

    if (-not $cwdHasManifest) {
        & cargo install `
            --git $repo `
            --branch $ref `
            --locked `
            --force `
            coding_agent_settings_backup
        if ($LASTEXITCODE -ne 0) { Fail 'cargo install failed' }
    }

    $bin = Join-Path $prefix 'bin\casb.exe'
    if (-not (Test-Path $bin)) {
        Fail "installation finished but $bin is missing"
    }
    Write-Step "installed: $bin"

    if (-not (Get-Command casb -ErrorAction SilentlyContinue)) {
        Write-Warn 'casb is not on PATH; add this to your $PROFILE:'
        Write-Warn ('    $env:Path = "{0}\bin;" + $env:Path' -f $prefix)
    }

    Write-Step 'running casb version'
    & $bin version

    Write-Host @'

Next steps:
  1. casb init                # create the backup root
  2. casb list                # see installed agents
  3. casb backup              # back up everything that's installed
  4. casb doctor              # verify the install

Configuration lives at $HOME\.config\casb\config.toml (run `casb config init`).
'@
}
