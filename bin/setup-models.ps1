# Model-server setup for Minerva's one native-Windows piece (thinkchiyo:
# Ryzen AI 7 PRO 350 / Radeon 860M / 64 GB).
#
# Fetches upstream llama.cpp's prebuilt Windows Vulkan build from the
# ggml-org/llama.cpp GitHub releases -- no cmake, no MSVC, no CUDA -- and,
# optionally, a Qwen3.8-27B quant. Everything else (eidolon, Aoide, jev, the
# web door, bench) lives in WSL and is not this script's business.
#
# Upstream, not the PrismML fork: the fork was only needed for the Bonsai
# P* ternary quants, which were dropped on 2026-09-26. Router mode and
# relative models.ini paths work on the stock build (docs/design/linux.md
# findings 8 and 9).
#
#   hoot setup                         binaries only (latest upstream release)
#   hoot setup -Tag b10883             pin a release instead of 'latest'
#   hoot setup -Force                  replace llama.cpp\ (e.g. the old Prism build)
#   hoot get UD-Q4_K_M                 a Qwen3.8-27B quant (= -QwenQuant)

[CmdletBinding()]
param(
    # Extra Qwen3.8-27B quant to fetch from unsloth/Qwen3.8-27B-GGUF. '' = skip.
    [string]$QwenQuant = '',
    # ggml-org/llama.cpp release tag to pull the win-vulkan-x64 zip from.
    # 'latest' asks the GitHub API which release that is today.
    [string]$Tag = 'latest',
    # Replace an existing llama.cpp\ instead of keeping it. The old one is
    # renamed to llama.cpp.prev, not deleted, so a bad release rolls back.
    [switch]$Force,
    # Weights only; leave llama.cpp\ alone (what `hoot get` passes).
    [switch]$SkipBinaries
)

$ErrorActionPreference = 'Stop'
# This file lives in bin\; the repo root is one up.
$root   = Split-Path $PSScriptRoot -Parent
$models = Join-Path $root 'models'
$bin    = Join-Path $root 'llama.cpp'
$prev   = Join-Path $root 'llama.cpp.prev'
$stamp  = Join-Path $bin 'RELEASE.txt'
$repo   = 'ggml-org/llama.cpp'
$hfQwen = 'https://huggingface.co/unsloth/Qwen3.8-27B-GGUF/resolve/main'

function Get-File($url, $out) {
    $name = Split-Path $out -Leaf
    # Remote size, so a half-finished file is resumed rather than mistaken for done.
    $want = $null
    try {
        $len = curl.exe -sIL $url 2>$null | Select-String -Pattern '^content-length:\s*(\d+)' |
               ForEach-Object { $_.Matches[0].Groups[1].Value } | Select-Object -Last 1
        if ($len) { $want = [int64]$len }
    } catch { $want = $null }

    if (Test-Path $out) {
        $have = (Get-Item $out).Length
        if ($want -and $have -eq $want) {
            Write-Host ("  have {0} ({1:N2} GB)" -f $name, ($have/1GB)) -ForegroundColor DarkGray; return
        }
        if ($want) {
            Write-Host ("  partial {0} ({1:N2} of {2:N2} GB) -> resuming" -f $name, ($have/1GB), ($want/1GB)) -ForegroundColor Yellow
        }
    }
    New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
    Write-Host "  GET $(Split-Path $out -Leaf)" -ForegroundColor Cyan
    # HF resets long transfers; -C - resumes, so just keep re-attaching.
    for ($i = 1; $i -le 20; $i++) {
        curl.exe -L --fail --retry 5 --retry-delay 5 --retry-all-errors -C - -o "$out" $url
        if ($LASTEXITCODE -eq 0 -and (-not $want -or (Get-Item $out).Length -eq $want)) { return }
        Write-Host "  ...dropped (attempt $i), resuming" -ForegroundColor Yellow
        Start-Sleep -Seconds 5
    }
    throw "download failed after 20 attempts: $url"
}

function Get-Release($tag) {
    # curl.exe rather than Invoke-RestMethod, same as Get-File: it carries
    # its own TLS stack, so PowerShell 5.1's protocol defaults never matter.
    $api = if ($tag -eq 'latest') { "https://api.github.com/repos/$repo/releases/latest" }
           else { "https://api.github.com/repos/$repo/releases/tags/$tag" }
    $json = curl.exe -sL --fail -H 'Accept: application/vnd.github+json' $api
    if ($LASTEXITCODE -ne 0 -or -not $json) { throw "GitHub API did not answer for $repo release '$tag': $api" }
    $rel = ($json -join "`n") | ConvertFrom-Json
    # Matched by suffix, not by a built name, so a change in upstream's
    # tag scheme does not break the lookup.
    $asset = $rel.assets | Where-Object { $_.name -match '-bin-win-vulkan-x64\.zip$' } | Select-Object -First 1
    if (-not $asset) { throw "release $($rel.tag_name) has no *-bin-win-vulkan-x64.zip asset" }
    [pscustomobject]@{ Tag = $rel.tag_name; Name = $asset.name; Url = $asset.browser_download_url }
}

# --- preflight ---------------------------------------------------------------
if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue)) { throw 'curl.exe not on PATH' }

$free = (Get-PSDrive $root.Substring(0,1)).Free / 1GB
Write-Host ("Free on {0}: {1:N1} GB" -f $root.Substring(0,1), $free)
if ($QwenQuant -and $free -lt 20) { throw ("Only {0:N1} GB free; a Qwen3.8-27B quant needs ~20 GB." -f $free) }

# --- binaries ----------------------------------------------------------------
# Vulkan build drives the Radeon 860M iGPU and still carries the CPU backend,
# so -ngl 0 falls back to CPU for any op the Vulkan path doesn't implement.
if (-not $SkipBinaries) {
    Write-Host "`n==> llama.cpp ($repo, prebuilt win-vulkan-x64)" -ForegroundColor Green
    $have = Test-Path (Join-Path $bin 'llama-server.exe')
    $from = if (Test-Path $stamp) { (Get-Content -Raw $stamp).Trim() } else { '' }
    if ($have -and $Force) {
        if (Test-Path $prev) { throw "$prev already exists; remove it, then re-run with -Force" }
        Write-Host "  moving the current build to $prev" -ForegroundColor Yellow
        Move-Item $bin $prev
        $have = $false
    }
    if (-not $have) {
        $rel = Get-Release $Tag
        Write-Host "  release $($rel.Tag)" -ForegroundColor Gray
        $zip = Join-Path $env:TEMP $rel.Name
        Get-File $rel.Url $zip
        New-Item -ItemType Directory -Force -Path $bin | Out-Null
        Expand-Archive -Path $zip -DestinationPath $bin -Force
        # Some release zips nest everything under build\bin\; flatten it.
        $nested = Get-ChildItem $bin -Recurse -Filter 'llama-server.exe' | Select-Object -First 1
        if ($nested -and $nested.DirectoryName -ne $bin) {
            Get-ChildItem $nested.DirectoryName | Move-Item -Destination $bin -Force
        }
        # Which build this is, for `hoot which` and the check below.
        [System.IO.File]::WriteAllText($stamp, "$repo $($rel.Tag)")
    } elseif ($from -notlike "$repo *") {
        # No stamp means the build predates this script: on this box, the
        # PrismML fork. It still serves the non-Bonsai models, but it is
        # not the build the setup decision names.
        Write-Host "  have llama-server.exe, but not from $repo (no $stamp)" -ForegroundColor Yellow
        Write-Host "  replace it with:  hoot setup -Force" -ForegroundColor Yellow
    } else {
        Write-Host "  have llama-server.exe ($from)" -ForegroundColor DarkGray
    }
}

# --- weights -----------------------------------------------------------------
# Router mode wants multi-file models in their own subdir, and the projector
# filename must start with "mmproj".
if ($QwenQuant) {
    $f = "Qwen3.8-27B-$QwenQuant.gguf"
    Write-Host "`n==> $f" -ForegroundColor Green
    $dir = Join-Path $models "Qwen3.8-27B-$QwenQuant"
    Get-File "$hfQwen/$f"             (Join-Path $dir $f)
    Get-File "$hfQwen/mmproj-F16.gguf" (Join-Path $dir 'mmproj-F16.gguf')
    Write-Host "  add a [Qwen3.8-27B-$QwenQuant] section to models.ini to serve it" -ForegroundColor Gray
}

# --- done --------------------------------------------------------------------
# models.ini is tracked in the repo, so this script never writes it.
Write-Host @"

Serve everything in models.ini (model list at http://127.0.0.1:8080):
  hoot serve        (foreground)   or   hoot up   (background)
"@ -ForegroundColor Gray
