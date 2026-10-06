# build.ps1 -- build a Minerva release: the Linux bundle, install.sh, minerva-install.exe, SHA256SUMS, latest.json.
# It does NOT sign and does NOT publish (see installer\README.md, "Releasing"). ASCII only (PowerShell 5.1).
#
#   installer\build.ps1 [-OutDir DIR] [-Version X.Y.Z] [-PubKey FILE] [-AllowDirty] [-AllowPlaceholderKey]
#
#   -OutDir               default installer\dist (git-ignored)
#   -Version              default: the repo-root VERSION file (the one source of truth)
#   -PubKey               the minisign PUBLIC key file compiled into the updater (default installer\minisign.pub;
#                         the build fails while that is the placeholder)
#   -AllowDirty           build even when the tracked sources differ from HEAD (a real release must not)
#   -AllowPlaceholderKey  tests only: compile in a key that makes every update fail closed
#   -Dev                  Linux build without musl-gcc (gcc against glibc headers, fortify off): a test build, never a release
#   -TestOverrides        compile in minerva's test-only back doors (MINERVA_KEYS_DIR, update --self-path): never a release
#
# Linux side (WSL): installer/build.sh with CARGO_TARGET_DIR=$HOME/installer-target. Windows side: cargo (MSVC).

[CmdletBinding()]
param(
    [string]$OutDir = '',
    [string]$Version = '',
    [string]$PubKey = '',
    [switch]$AllowDirty,
    [switch]$AllowPlaceholderKey,
    [switch]$Dev,
    [switch]$TestOverrides
)
# Native programs write progress to stderr; failures are caught by their exit codes below.
$ErrorActionPreference = 'Continue'
$Repo = Split-Path $PSScriptRoot -Parent
if (-not $OutDir) { $OutDir = Join-Path $PSScriptRoot 'dist' }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

function Wsl-Path([string]$p) {
    $o = @(& wsl.exe -e wslpath -a $p)
    if ($LASTEXITCODE -ne 0 -or -not $o) { throw "wslpath failed for $p" }
    return "$($o[0])".Trim()
}

# ---- version: VERSION + short sha (not stored anywhere) ----
$base = if ($Version) { $Version } else { (Get-Content -Raw (Join-Path $Repo 'VERSION')).Trim() }
$sha = (& git -C $Repo rev-parse --short HEAD).Trim()
$dirty = @(& git -C $Repo status --porcelain --untracked-files=all -- perch setup eidolon harnox webui/term bin installer jev eidolon-plugins nix flake.nix flake.lock VERSION)
if ($dirty.Count -gt 0) {
    if (-not $AllowDirty) { throw "tracked sources differ from HEAD ($($dirty.Count) paths); commit first, or -AllowDirty for a test build" }
    $sha = "$sha.dirty"
}
$full = "$base-g$sha"
Write-Host "== Minerva $full -> $OutDir"

# ---- Linux bundle (WSL) ----
$bargs = @('--out', (Wsl-Path $OutDir), '--version-base', $base, '--sha', $sha)
if ($PubKey) { $bargs += @('--pubkey', (Wsl-Path (Resolve-Path $PubKey).Path)) }
if ($AllowPlaceholderKey) { $bargs += '--allow-placeholder-key' }
if ($Dev) { $bargs += '--dev' }
if ($TestOverrides) { $bargs += '--test-overrides' }
# the script path and every argument reach bash as separate, quoted words (a path with a space stays one)
& wsl.exe -e bash -lc '\"$0\" \"$@\"' ((Wsl-Path $Repo) + '/installer/build.sh') @bargs
if ($LASTEXITCODE -ne 0) { throw 'the Linux build failed' }
$bundle = Join-Path $OutDir "minerva-$full-linux-x86_64.tar.gz"
if (-not (Test-Path $bundle)) { throw "missing $bundle" }

# ---- Windows exe (carries the bundle) ----
$env:MINERVA_BUNDLE = $bundle
$env:MINERVA_VERSION_BASE = $base
$env:MINERVA_BUILD_SHA = $sha
if ($PubKey) { $env:MINERVA_UPDATE_PUBKEY = (Resolve-Path $PubKey).Path }
if ($AllowPlaceholderKey) { $env:MINERVA_ALLOW_PLACEHOLDER_KEY = '1' }
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $PSScriptRoot 'target' }
Push-Location $PSScriptRoot
try {
    & cargo build --release --locked --bin minerva-install
    if ($LASTEXITCODE -ne 0) { throw 'the Windows build failed' }
} finally { Pop-Location }
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\minerva-install.exe') (Join-Path $OutDir 'minerva-install.exe') -Force

# ---- sums + manifest ----
& wsl.exe -e bash -lc '\"$0\" \"$@\"' ((Wsl-Path $Repo) + '/installer/release-files.sh') (Wsl-Path $OutDir)
if ($LASTEXITCODE -ne 0) { throw 'release-files.sh failed' }

Write-Host ''
Write-Host 'Built (not signed, not published):'
Get-ChildItem $OutDir -File | ForEach-Object { '  {0,12:N0}  {1}' -f $_.Length, $_.Name }
Write-Host ''
Write-Host 'Next, the operator: sign latest.json and the bundle, then publish. See installer\README.md, "Releasing".'
