# install.ps1 — One-liner installer for casb (coding_agent_settings_backup) on Windows.
#
# Usage (pipe-safe — no [CmdletBinding]/param so iex works):
#   irm "https://raw.githubusercontent.com/quangdang46/coding_agent_settings_backup/main/install.ps1" | iex
#
# Options:
#   -Dest PATH         Install binary to PATH (default: $env:USERPROFILE\.casb\bin)
#   -Version VER      Specific version to install (e.g. v0.1.0)
#   -System           Install to C:\Program Files\casb
#   -Verify           Run self-test after install
#   -FromSource       Build from source instead of downloading
#   -Quiet            Suppress non-error output
#   -Uninstall        Remove installed binary

param(
    [string]$Dest,
    [string]$Version,
    [switch]$System,
    [switch]$Verify,
    [switch]$FromSource,
    [switch]$Quiet,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$BINARY_NAME = 'casb'
$OWNER = 'quangdang46'
$REPO = 'coding_agent_settings_backup'

function Write-Info {
    param([string]$Message)
    if (-not $Quiet) { Write-Host "[${BINARY_NAME}] $Message" -ForegroundColor Cyan }
}

function Write-Warn {
    param([string]$Message)
    Write-Host "[${BINARY_NAME}] WARN: $Message" -ForegroundColor Yellow
}

function Write-Success {
    param([string]$Message)
    if (-not $Quiet) { Write-Host "✓ $Message" -ForegroundColor Green }
}

function Fail {
    param([string]$Message)
    Write-Host "ERROR: $Message" -ForegroundColor Red
    throw $Message
}

function Get-TempDir {
    Join-Path $env:TEMP "casb-install-$([guid]::NewGuid().ToString('N'))"
}

# === Uninstall ===
if ($Uninstall) {
    $prefix = if ($Dest) { $Dest } else { Join-Path $env:USERPROFILE ".${BINARY_NAME}\bin" }
    $bin = Join-Path $prefix "$BINARY_NAME.exe"
    if (Test-Path $bin) {
        Remove-Item $bin -Force
        Write-Success "${BINARY_NAME}.exe uninstalled from $prefix"
    } else {
        Write-Info "Not installed at $prefix"
    }
    exit 0
}

# === Resolve defaults ===
if (-not $Dest) {
    if ($System) {
        $Dest = "C:\Program Files\$BINARY_NAME"
    } else {
        $Dest = Join-Path $env:USERPROFILE ".${BINARY_NAME}\bin"
    }
}

if (-not (Test-Path $Dest)) {
    New-Item -ItemType Directory -Path $Dest -Force | Out-Null
}

# === Resolve version ===
function Resolve-Version {
    if ($Version) { return $Version }
    
    Write-Info 'resolving latest release ...'
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/${OWNER}/${REPO}/releases/latest" -UseBasicParsing
        return $release.tag_name
    } catch {
        Fail "could not fetch latest release: $_"
    }
}

# === Download ===
function Download-File {
    param([string]$Url, [string]$DestPath)
    
    $attempt = 0
    $maxRetries = 3
    while ($attempt -lt $maxRetries) {
        $attempt++
        try {
            Write-Info "Downloading ($attempt/$maxRetries)..."
            Invoke-WebRequest -Uri $Url -OutFile $DestPath -UseBasicParsing
            return $true
        } catch {
            if ($attempt -lt $maxRetries) {
                Write-Warn "Retry in 3s..."
                Start-Sleep -Seconds 3
            } else {
                return $false
            }
        }
    }
    return $false
}

# === SHA256 verify ===
function Verify-Checksum {
    param([string]$File, [string]$Url)
    
    try {
        $sha256Url = "${Url}.sha256"
        $resp = Invoke-WebRequest -Uri $sha256Url -UseBasicParsing
        $raw = if ($resp.Content -is [byte[]]) {
            [System.Text.Encoding]::UTF8.GetString($resp.Content)
        } else {
            $resp.Content
        }
        $expectedHash = $raw.Trim().Split(' ')[0].ToLower()
        $actualHash = (Get-FileHash -Path $File -Algorithm SHA256).Hash.ToLower()
        
        if ($actualHash -ne $expectedHash) {
            Fail "SHA-256 mismatch: expected $expectedHash, got $actualHash"
        }
        Write-Success 'Checksum verified'
        return $true
    } catch {
        Write-Warn 'Checksum file not available — skipping verification'
        return $false
    }
}

# === Build from source ===
function Build-FromSource {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail "cargo not found — install Rust: https://rustup.rs"
    }
    
    Write-Info 'Building from source...'
    $tmp = $script:TMP
    
    try {
        git clone --depth 1 "https://github.com/${OWNER}/${REPO}.git" (Join-Path $tmp 'src')
        $env:CARGO_TARGET_DIR = Join-Path $tmp 'target'
        Push-Location (Join-Path $tmp 'src')
        try {
            cargo build --release
        } finally {
            Pop-Location
        }
        
        $srcBin = Join-Path $tmp "target\release\$BINARY_NAME.exe"
        if (-not (Test-Path $srcBin)) {
            Fail "Build failed - binary not found"
        }
        
        $destBin = Join-Path $Dest "$BINARY_NAME.exe"
        Copy-Item -Path $srcBin -Destination $destBin -Force
        Write-Success "installed: $destBin"
    } catch {
        Fail "Build failed: $_"
    }
}

# === Main ===
$TMP = Get-TempDir
New-Item -ItemType Directory -Path $TMP -Force | Out-Null

try {
    $ver = Resolve-Version
    Write-Info "Version: $ver"
    
    if (-not $FromSource) {
        $archiveName = "casb-windows-x86_64.zip"
        $downloadUrl = "https://github.com/${OWNER}/${REPO}/releases/download/${ver}/${archiveName}"
        $zipPath = Join-Path $TMP $archiveName
        
        if (Download-File -Url $downloadUrl -DestPath $zipPath) {
            Verify-Checksum -File $zipPath -Url $downloadUrl
            
            Write-Info "Extracting..."
            $extractDir = Join-Path $TMP 'extract'
            Expand-Archive -Path $zipPath -DestinationPath $extractDir -Force
            
            $exe = Get-ChildItem -Path $extractDir -Filter '*.exe' -Recurse | Select-Object -First 1
            if (-not $exe) { Fail 'casb.exe not found in archive' }
            
            $destBin = Join-Path $Dest "$BINARY_NAME.exe"
            Copy-Item -Path $exe.FullName -Destination $destBin -Force
            Write-Success "installed: $destBin"
        } else {
            Write-Warn "Binary download failed — will try building from source"
            Build-FromSource
        }
    } else {
        Build-FromSource
    }
    
    # PATH
    $binPath = Join-Path $Dest "$BINARY_NAME.exe"
    if (-not (Get-Command $binPath -ErrorAction SilentlyContinue)) {
        $env:Path = "$Dest;$env:Path"
        
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        if ($userPath -notlike "*$Dest*") {
            [Environment]::SetEnvironmentVariable('Path', "$Dest;$userPath", 'User')
            Write-Info "Added $Dest to User PATH (takes effect in new terminals)"
        }
    }
    
    if ($Verify) {
        & $binPath --version
    }
    
    Write-Host ""
    Write-Success "$BINARY_NAME installed → $binPath"
    Write-Host ""
    Write-Host "  Next steps:"
    Write-Host "    $BINARY_NAME init                # create the backup root"
    Write-Host "    $BINARY_NAME list                # see installed agents"
    Write-Host "    $BINARY_NAME backup              # back up everything"
    Write-Host ""
    
} finally {
    Remove-Item $TMP -Recurse -Force -ErrorAction SilentlyContinue
}
