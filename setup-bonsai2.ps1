# Ternary-Bonsai-2-27B + Qwen3.8-27B on thinkchiyo (Ryzen AI 7 PRO 350 / Radeon 860M / 64 GB)
#
# Uses the PrismML prebuilt Windows Vulkan binaries -- no cmake, no MSVC, no CUDA.
# (This box has no NVIDIA GPU, so the original from-source CUDA build could never work.)
# Stock llama.cpp still cannot load the P* ternary quants; the Prism fork can.
#
#   cd C:\Users\dxcen\Projects\bonsai2
#   powershell -ExecutionPolicy Bypass -File .\setup-bonsai2.ps1
#   powershell -ExecutionPolicy Bypass -File .\setup-bonsai2.ps1 -QwenQuant UD-Q4_K_M

[CmdletBinding()]
param(
    # Extra Qwen3.8-27B quant to fetch from unsloth/Qwen3.8-27B-GGUF. '' = skip.
    [string]$QwenQuant = '',
    # Prism release to pull prebuilt binaries from.
    [string]$PrismTag  = 'prism-b10685-7dffb15',
    [switch]$SkipBonsai
)

$ErrorActionPreference = 'Stop'
$root   = $PSScriptRoot
$models = Join-Path $root 'models'
$bin    = Join-Path $root 'llama.cpp'
$hf     = 'https://huggingface.co/prism-ml/Ternary-Bonsai-2-27B-gguf/resolve/main'
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

# --- preflight ---------------------------------------------------------------
if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue)) { throw 'curl.exe not on PATH' }

$free = (Get-PSDrive $root.Substring(0,1)).Free / 1GB
Write-Host ("Free on {0}: {1:N1} GB" -f $root.Substring(0,1), $free)
if ($free -lt 15) { throw ("Only {0:N1} GB free; need ~15 GB minimum." -f $free) }

# --- binaries ----------------------------------------------------------------
# Vulkan build drives the Radeon 860M iGPU and still carries the CPU backend,
# so -ngl 0 falls back to CPU for any op the Vulkan path doesn't implement.
Write-Host "`n==> llama.cpp (Prism fork, prebuilt win-vulkan-x64)" -ForegroundColor Green
$zipName = "llama-$PrismTag-bin-win-vulkan-x64.zip"
$zip     = Join-Path $env:TEMP $zipName
if (-not (Test-Path (Join-Path $bin 'llama-server.exe'))) {
    Get-File "https://github.com/PrismML-Eng/llama.cpp/releases/download/$PrismTag/$zipName" $zip
    New-Item -ItemType Directory -Force -Path $bin | Out-Null
    Expand-Archive -Path $zip -DestinationPath $bin -Force
    # release zips nest everything under build\bin\; flatten it
    $nested = Get-ChildItem $bin -Recurse -Filter 'llama-server.exe' | Select-Object -First 1
    if ($nested -and $nested.DirectoryName -ne $bin) {
        Get-ChildItem $nested.DirectoryName | Move-Item -Destination $bin -Force
    }
} else {
    Write-Host "  have llama-server.exe" -ForegroundColor DarkGray
}

# --- weights -----------------------------------------------------------------
# Router mode wants multi-file models in their own subdir, and the projector
# filename must start with "mmproj".
if (-not $SkipBonsai) {
    Write-Host "`n==> Ternary-Bonsai-2-27B PTQ1_0 (5.95 GB, 1.75 bpw)" -ForegroundColor Green
    $dir = Join-Path $models 'Ternary-Bonsai-2-27B-PTQ1_0'
    Get-File "$hf/Ternary-Bonsai-2-27B-PTQ1_0.gguf"   (Join-Path $dir 'Ternary-Bonsai-2-27B-PTQ1_0.gguf')
    Get-File "$hf/Ternary-Bonsai-2-27B-mmproj-Q8_0.gguf" (Join-Path $dir 'mmproj-Q8_0.gguf')
}

if ($QwenQuant) {
    $f = "Qwen3.8-27B-$QwenQuant.gguf"
    Write-Host "`n==> $f" -ForegroundColor Green
    $dir = Join-Path $models "Qwen3.8-27B-$QwenQuant"
    Get-File "$hfQwen/$f"             (Join-Path $dir $f)
    Get-File "$hfQwen/mmproj-F16.gguf" (Join-Path $dir 'mmproj-F16.gguf')
}

# --- model list --------------------------------------------------------------
# llama-server router mode reads this and exposes every entry on GET /v1/models,
# which is also what the built-in WebUI's model picker and any OpenAI-compatible
# client (Cline, Open WebUI, ...) will show.
$ini = Join-Path $root 'models.ini'
if (-not (Test-Path $ini)) {
    @"
version = 1

[*]
c = 16384
jinja = true
flash-attn = on
; Measured: bandwidth-bound past 8 threads (16 threads is ~23% SLOWER).
threads = 8

[bonsai-2-27b]
model = $models\Ternary-Bonsai-2-27B-PTQ1_0\Ternary-Bonsai-2-27B-PTQ1_0.gguf
mmproj = $models\Ternary-Bonsai-2-27B-PTQ1_0\mmproj-Q8_0.gguf
n-gpu-layers = 99
c = 32768
temp = 1.0
top-p = 0.95
top-k = 20
load-on-startup = true
"@ | ForEach-Object { [System.IO.File]::WriteAllText($ini, $_) }   # no BOM; the INI parser trips on it
    Write-Host "`nWrote $ini" -ForegroundColor Green
} else {
    Write-Host "`nKept existing $ini" -ForegroundColor DarkGray
}

# --- done --------------------------------------------------------------------
Write-Host @"

Serve everything in models.ini (model list at http://127.0.0.1:8080):
  .\serve.ps1

One-off, single model:
  .\llama.cpp\llama-server.exe -m .\models\Ternary-Bonsai-2-27B-PTQ1_0\Ternary-Bonsai-2-27B-PTQ1_0.gguf ``
      --mmproj .\models\Ternary-Bonsai-2-27B-PTQ1_0\mmproj-Q8_0.gguf ``
      -ngl 99 -fa on -c 32768 --temp 1.0 --top-p 0.95 --top-k 20 --port 8080

If Vulkan chokes on a PTQ1_0 op, re-run with -ngl 0 to stay on CPU.
"@ -ForegroundColor Gray
