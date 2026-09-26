# hoot -- the model-server launcher for Minerva
#
# Strictly 7-bit ASCII on purpose: PowerShell 5.1 reads .ps1 as ANSI unless the
# file carries a BOM, so box-drawing glyphs turn to mojibake. ASCII always works.
#
# The ONE native-Windows piece of Minerva is llama-server (upstream
# ggml-org/llama.cpp, Vulkan build) in router mode. Everything else --
# eidolon, Aoide, jev, the web door (webui/term), bench -- runs in WSL and
# is started there, not from here (decided 2026-09-26).
#
#   hoot serve [-Port N] [-Bind ADDR]   start llama-server (router mode), foreground
#   hoot up    [-Port N] [-Bind ADDR]   the same, backgrounded; waits for the API
#   hoot stop                           stop llama-server (and its model children)
#   hoot status                         is it up, and which models it serves
#   hoot models                         list models from models.ini / the API
#   hoot get <quant>                    download a Qwen3.8-27B quant
#   hoot setup [-Tag T] [-Force]        fetch upstream llama.cpp (win-vulkan-x64)
#   hoot edit                           open models.ini
#   hoot which                          show which llama.cpp binaries are in use

[CmdletBinding()]
param(
    [Parameter(Position = 0)][string]$Command = 'help',
    [Parameter(Position = 1, ValueFromRemainingArguments = $true)][string[]]$Rest,
    [int]$Port = 8080,
    [string]$Bind = '127.0.0.1',
    # 'setup' only, handed to setup-models.ps1: a ggml-org/llama.cpp release
    # tag (default: whatever GitHub calls latest), and whether to replace an
    # existing llama.cpp\ build. Declared here because an advanced script
    # refuses -Names it does not know, even with a remaining-args catch-all.
    [string]$Tag = 'latest',
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$Root   = Split-Path $PSScriptRoot -Parent
$Bin    = Join-Path $Root 'llama.cpp'
$Ini    = Join-Path $Root 'models.ini'
$Models = Join-Path $Root 'models'
$Cache  = Join-Path $Root '.cache'
$Setup  = Join-Path $PSScriptRoot 'setup-models.ps1'
# Where hoot itself probes the API. A wildcard bind (0.0.0.0, the NAT-mode
# WSL route -- see providers\hoot.rn) is not an address Windows can connect
# to, so probe loopback in that case; the server listens there too.
$Probe  = if ($Bind -in '0.0.0.0', '::', '[::]') { '127.0.0.1' } else { $Bind }
$Base   = "http://${Probe}:${Port}"

$W = 66

# ---- ascii furniture --------------------------------------------------------
function Banner {
    $art = @'
       ___       ___       ___       ___
      /\__\     /\  \     /\  \     /\  \
     /:/__/_   /::\  \   /::\  \    \:\  \
    /::\/\__\ /:/\:\__\ /:/\:\__\   /::\__\
    \/\::/  / \:\/:/  / \:\/:/  /  /:/\/__/
      /:/  /   \::/  /   \::/  /   \/__/
      \/__/     \/__/     \/__/
'@
    Write-Host $art -ForegroundColor DarkCyan
    Write-Host '        ,___,   Make it, Break it, Hack it.' -ForegroundColor DarkGray
    Write-Host '        [O.o]' -ForegroundColor DarkGray
    Write-Host '        /)_)' -ForegroundColor DarkGray
    Write-Host '         ""' -ForegroundColor DarkGray
    Write-Host ''
}

function Box($title, [string[]]$lines, $color = 'Cyan') {
    $t = "[ $title ]"
    Write-Host ("  +--$t" + ("-" * [Math]::Max(0, $W - $t.Length - 2)) + "+") -ForegroundColor $color
    foreach ($l in $lines) {
        $s = "$l"
        if ($s.Length -gt ($W - 2)) { $s = $s.Substring(0, $W - 5) + '...' }
        Write-Host ("  | " + $s + (" " * [Math]::Max(0, $W - $s.Length - 1)) + "|") -ForegroundColor Gray
    }
    Write-Host ("  +" + ("-" * $W) + "+") -ForegroundColor $color
}

function Say  ($m) { Write-Host "  >> $m" -ForegroundColor Cyan }
function Ok   ($m) { Write-Host "  ok $m" -ForegroundColor Green }
function Warn ($m) { Write-Host "  !! $m" -ForegroundColor Yellow }
function Die  ($m) { Write-Host "  xx hoot: $m" -ForegroundColor Red; exit 1 }
function Dot  ($up) { if ($up) { '[#]' } else { '[ ]' } }

# ---- helpers ----------------------------------------------------------------
function Get-Server {
    Get-Process llama-server -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($Bin) }
}
function Test-Api($url) { try { Invoke-RestMethod $url -TimeoutSec 3 | Out-Null; $true } catch { $false } }

function Wait-Api($url, $timeoutSec, $label) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $spin = '|/-\'; $i = 0
    while ($sw.Elapsed.TotalSeconds -lt $timeoutSec) {
        if (Test-Api $url) { Write-Host ("`r  ok $label up ({0}s)            " -f [int]$sw.Elapsed.TotalSeconds) -ForegroundColor Green; return $true }
        Write-Host ("`r  {0}  waiting for {1} ... {2}s   " -f $spin[$i++ % 4], $label, [int]$sw.Elapsed.TotalSeconds) -NoNewline -ForegroundColor DarkGray
        Start-Sleep -Seconds 2
    }
    Write-Host ("`r  xx {0} did not answer in {1}s     " -f $label, $timeoutSec) -ForegroundColor Red
    return $false
}

function Get-Release {
    # Written by setup-models.ps1 after it unpacks a release; absent on a
    # build that predates it (on this box: the PrismML fork).
    $stamp = Join-Path $Bin 'RELEASE.txt'
    if (Test-Path $stamp) { return (Get-Content -Raw $stamp).Trim() }
    return '(unknown -- not installed by hoot setup)'
}

function Assert-Serveable {
    if (-not (Test-Path (Join-Path $Bin 'llama-server.exe'))) { Die 'no binaries; run: hoot setup' }
    if (-not (Test-Path $Ini)) { Die "missing $Ini" }
}

switch ($Command.ToLower()) {

    'serve' {
        Banner
        if (Get-Server) { Die 'already running (hoot stop, or hoot status)' }
        Assert-Serveable
        New-Item -ItemType Directory -Force -Path $Cache | Out-Null
        # Isolated cache so the router lists only OUR models, not stray
        # half-cached entries left by the winget llama.cpp install.
        $env:LLAMA_CACHE = $Cache
        Box 'router' @("api     $Base/v1", "webui   $Base", "config  $Ini", '', 'ctrl-c to stop')
        Write-Host ''
        # models.ini names its files relative to the router's working
        # directory (docs/design/linux.md finding 9), so run it from the
        # repo root whichever directory `hoot` was typed in.
        # Absolute path on purpose: a bare llama-server resolves to whatever
        # is on PATH (the winget build), not the release hoot setup fetched.
        Push-Location $Root
        try {
            & (Join-Path $Bin 'llama-server.exe') `
                --models-preset $Ini --models-max 2 --host $Bind --port $Port @Rest
        } finally { Pop-Location }
    }

    'up' {
        # 'serve', backgrounded: start the router hidden, wait for its API,
        # then show status. Nothing else -- the rest of Minerva is WSL's.
        Banner
        if (Get-Server) { Ok 'router already up' }
        else {
            Assert-Serveable
            Say 'starting router'
            New-Item -ItemType Directory -Force -Path $Cache | Out-Null
            $env:LLAMA_CACHE = $Cache
            # -WorkingDirectory for the same reason 'serve' pushes $Root.
            Start-Process -FilePath (Join-Path $Bin 'llama-server.exe') `
                -ArgumentList '--models-preset', $Ini, '--models-max', '2', '--host', $Bind, '--port', $Port `
                -WorkingDirectory $Root -WindowStyle Hidden
            if (-not (Wait-Api "$Base/v1/models" 600 'router')) { Die 'router failed to start (hoot serve shows why)' }
        }
        Write-Host ''
        & $PSCommandPath status -Port $Port -Bind $Bind
    }

    'stop' {
        Banner
        $killed = 0
        # The router spawns a child process per loaded model; killing the parent
        # alone leaves the child holding the port. Sweep until the list is empty.
        foreach ($pass in 1..6) {
            $p = @(Get-Server)
            if (-not $p) { break }
            $p | Stop-Process -Force -ErrorAction SilentlyContinue
            $killed += $p.Count
            Start-Sleep -Milliseconds 800
        }
        if (Get-Server) { Die 'llama-server still running' }
        if ($killed) { Ok ("stopped router ({0} proc)" -f $killed) }
        else { Warn 'nothing was running' }
    }

    'status' {
        Banner
        $srvUp = Test-Api "$Base/v1/models"
        $procs = @(Get-Server)
        Box 'status' @(
            ("{0} router      {1,-24} {2}" -f (Dot $srvUp), $Base, $(if ($srvUp) { 'responding' } else { 'not responding' }))
            ("    processes   {0}" -f $procs.Count)
            ("    build       {0}" -f (Get-Release))
        )
        if ($srvUp) {
            try {
                $m = (Invoke-RestMethod "$Base/v1/models" -TimeoutSec 10).data
                # Router mode reports per-model state where the build has it;
                # older builds list ids only.
                Write-Host ''
                Box 'served' ($m | ForEach-Object {
                    $st = if ($_.status -and $_.status.value) { " ($($_.status.value))" } else { '' }
                    "  * $(if ($_.name) { $_.name } else { $_.id })$st"
                })
            } catch { Warn 'router process up but /v1/models not answering yet' }
        }
    }

    'models' {
        Banner
        if (Get-Server) {
            $m = (Invoke-RestMethod "$Base/v1/models" -TimeoutSec 5).data
            Box 'served now' ($m | ForEach-Object { "  * $(if ($_.name) { $_.name } else { $_.id })" })
        } else {
            $secs = Select-String -Path $Ini -Pattern '^\[(?!\*)(.+)\]' |
                    ForEach-Object { "  * $($_.Matches[0].Groups[1].Value)" }
            Box 'configured (models.ini)' $secs
            Write-Host ''
            $files = Get-ChildItem $Models -Recurse -Filter *.gguf -ErrorAction SilentlyContinue |
                     Where-Object { $_.Name -notlike 'mmproj*' } |
                     ForEach-Object { "  {0,8:N2} GB  {1}" -f ($_.Length / 1GB), $_.Name }
            Box 'on disk' $files 'DarkGray'
        }
    }

    'get' {
        Banner
        if (-not $Rest -or -not $Rest[0]) { Die 'usage: hoot get <quant>   e.g. hoot get UD-Q5_K_M' }
        & $Setup -QwenQuant $Rest[0] -SkipBinaries
    }

    'setup' {
        # Binaries (upstream llama.cpp, win-vulkan-x64) and, when asked,
        # weights. Nothing about eidolon: it lives in WSL now, and its own
        # setup (extensions_dir, providers) is done there.
        Banner
        & $Setup -Tag $Tag -Force:$Force
    }

    'edit' { Start-Process notepad.exe $Ini }

    'which' {
        Banner
        $other = Get-Command llama-server.exe -ErrorAction SilentlyContinue
        Box 'paths' @(
            "hoot uses   $Bin\llama-server.exe"
            "build       $(Get-Release)"
            "on PATH     $(if ($other) { $other.Source } else { '(none)' })"
            "models.ini  $Ini"
            "models      $Models"
            "cache       $Cache"
        )
        if ($other -and -not $other.Source.StartsWith($Bin)) {
            Write-Host ''
            Warn 'a different llama-server is on PATH; hoot ignores it and runs its own'
        }
    }

    { $_ -in 'shim', 'webui', 'aoide', 'theme', 'bench', 'jev' } {
        # Removed 2026-09-26, when hoot became the model-server launcher
        # only. Kept as a catch for old muscle memory, the way 'jev' was.
        Banner
        switch ($_) {
            'webui' { Die 'webui moved: Open WebUI is retired; the face is webui/term, served by eidolon web in WSL' }
            'bench' { Die 'bench moved: llama.cpp\llama-bench.exe directly, or the bench loop in WSL' }
            default { Die "$_ moved: eidolon, Aoide and jev run in WSL now -- start them there" }
        }
    }

    default {
        Banner
        Box 'commands' @(
            'serve                 start the llama.cpp router (foreground)'
            'up                    start the router in the background'
            'stop                  stop the router'
            'status                is it up + models served'
            'models                list models (served or configured)'
            'get <quant>           fetch a Qwen3.8-27B quant, e.g. UD-Q5_K_M'
            'setup [-Tag] [-Force] fetch upstream llama.cpp (win-vulkan-x64)'
            'edit                  edit models.ini'
            'which                 show binaries/config in use'
        )
        Write-Host ''
        Write-Host "   api  $Base/v1   (eidolon, Aoide, jev, webui/term: in WSL)" -ForegroundColor DarkGray
        Write-Host ''
    }
}
