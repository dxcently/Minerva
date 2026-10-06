# hoot -- opens Minerva, and launches its model server
#
# Strictly 7-bit ASCII on purpose: PowerShell 5.1 reads .ps1 as ANSI unless the
# file carries a BOM, so box-drawing glyphs turn to mojibake. ASCII always works.
#
# The ONE native-Windows piece of Minerva is llama-server (upstream
# ggml-org/llama.cpp, Vulkan build) in router mode. Everything else --
# eidolon, Aoide, jev, the Minerva server (webui/term), bench -- runs in WSL.
# `hoot` alone starts (or reuses) the server there through wsl.exe and opens
# the browser, already signed in (a 30 s single-use link; no password, no
# token in the URL). It does not start llama-server unless you ask (hoot up).
#
#   hoot                                open Minerva in the browser (same as: hoot web);
#                                       the very first time, the setup wizard runs first
#   hoot init                           the setup wizard (home, default model, project roots); run it again any time
#   hoot web [-NoOpen]                  the same; -NoOpen prints the one-time link instead
#   hoot status web                     is the Minerva server up, and its sessions
#   hoot stop web                       stop the Minerva server (chats are kept)
#   hoot serve [-Port N] [-Bind ADDR]   start llama-server (router mode), foreground
#   hoot up    [-Port N] [-Bind ADDR]   the same, backgrounded; waits for the API
#   hoot stop                           stop llama-server (and its model children)
#   hoot status                         is it up, and which models it serves
#   hoot models                         list models from models.ini / the API
#   hoot get <quant>                    download a Qwen3.8-27B quant
#   hoot setup [-Tag T] [-Force]        fetch upstream llama.cpp (win-vulkan-x64)
#   hoot edit                           open models.ini
#   hoot which                          show which llama.cpp binaries are in use
#   hoot help                           this list
#   hoot update [-Check] [-Rollback]    installed copies only: install the newest signed release / only look / go back one
#   hoot keys list|inspect|add|remove   installed copies only: who may sign releases (runs `minerva keys` in WSL)
#   hoot jev-setup [--dry-run]          installed copies only: the optional jev chooser (Python venv, CPU torch, ~1.2 GB)
#   hoot version                        installed copies only: the hoot and bundle versions
#
# Two ways to run: from a repo checkout (dev: it finds perch/eidolon/webui in the checkout) or INSTALLED by
# minerva-install.exe. Installed mode is on when a file `install.json` sits next to this script (the installer writes
# it: the WSL distro and prefix). Then the programs come from <prefix>/opt/minerva/current inside that distro, and
# the llama-server verbs (serve, up, models, ...) are not available (they need a checkout with models.ini).
# An installed hoot refreshes itself: when install.json's version differs from the installed bundle's (the Linux side
# was updated by `minerva update` or the installer), hoot copies the bundle's hoot files over itself and asks you to run
# the command again. A checkout never does this.

[CmdletBinding()]
param(
    [Parameter(Position = 0)][string]$Command = '',
    [Parameter(Position = 1, ValueFromRemainingArguments = $true)][string[]]$Rest,
    [int]$Port = 8080,
    [string]$Bind = '127.0.0.1',
    # 'setup' only, handed to setup-models.ps1: a ggml-org/llama.cpp release
    # tag (default: whatever GitHub calls latest), and whether to replace an
    # existing llama.cpp\ build. Declared here because an advanced script
    # refuses -Names it does not know, even with a remaining-args catch-all.
    [string]$Tag = 'latest',
    [switch]$Force,
    # 'web' only: print the one-time link instead of opening the browser.
    [switch]$NoOpen,
    # 'update' only (installed copies).
    [switch]$Check,
    [switch]$Rollback
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

# Installed mode (see the header): install.json = {distro, wsl_prefix, version}, written by minerva-install.exe.
$Installed = $false
$WslD = @()
$OptCur = $null
$Marker = Join-Path $PSScriptRoot 'install.json'
if (Test-Path $Marker) {
    $Inst = Get-Content -Raw $Marker | ConvertFrom-Json
    if ($Inst.distro -and $Inst.wsl_prefix) {
        # install.json is a file in a folder on PATH; whatever is in it ends up inside a bash command line. The prefix must
        # be an absolute Linux path with no quote and no control character (the same bar dev mode sets for the checkout path),
        # and the distro name plain.
        if ([string]$Inst.wsl_prefix -notmatch '^/[^\x00-\x1F\x7F''"`$\\]*$' -or ([string]$Inst.wsl_prefix).Contains('..')) {
            Write-Host '  xx hoot: install.json has a wsl_prefix that is not a plain absolute Linux path; refusing to use it. Reinstall with minerva-install.exe.' -ForegroundColor Red
            exit 1
        }
        if ([string]$Inst.distro -notmatch '^[A-Za-z0-9._ -]{1,64}$') {
            Write-Host '  xx hoot: install.json has a distro name with odd characters; refusing to use it. Reinstall with minerva-install.exe.' -ForegroundColor Red
            exit 1
        }
        $Installed = $true
        $WslD = @('-d', [string]$Inst.distro)
        $OptCur = "$($Inst.wsl_prefix)/opt/minerva/current"
    }
}

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

# ---- Minerva (the WSL server) -----------------------------------------------
# bin/perch-open.sh in WSL does the work; hoot only calls it and opens one URL.
# WSL does the mapping (wslpath), so a custom automount root in wsl.conf works too; the path goes as an
# argument, never through a shell.
function ConvertTo-WslPath($p) {
    # Collect all output before taking the first line: Select-Object -First 1 would stop the pipeline
    # early, kill wsl.exe and leave a non-zero $LASTEXITCODE behind.
    $out = @(& wsl.exe @WslD -e wslpath -a $p)
    if ($LASTEXITCODE -ne 0 -or -not $out) { Die "cannot map $p into WSL (wslpath failed)" }
    $path = "$($out[0])".Trim()
    if ($path.Contains("'")) { Die 'the repo path has a quote in it; move the checkout' }
    return $path
}

# Settings that may be overridden from Windows (a second server on another port, a test home).
# Passed through WSLENV, so values are never pasted into a command line. They arrive unchanged, so
# they must already be WSL paths (/home/you/Minerva), not Windows ones.
function Set-WslEnv {
    $names = @('MINERVA_PERCH_PORT', 'MINERVA_HOME', 'MINERVA_PERCH_BIN', 'MINERVA_UI_DIR', 'MINERVA_SETUP_BIN', 'MINERVA_NO_UPDATE_CHECK') | Where-Object { Test-Path "env:$_" }
    if ($names) {
        $have = @($env:WSLENV -split ':' | Where-Object { $_ })
        # Compared by name with any /flags stripped: `MINERVA_HOME/p` already lists MINERVA_HOME.
        $listed = @($have | ForEach-Object { ($_ -split '/')[0] })
        $env:WSLENV = (@($have) + @($names | Where-Object { $listed -notcontains $_ })) -join ':'
    }
}

function Invoke-PerchOpen([string]$flag) {
    Set-WslEnv
    $script = if ($Installed) { "$OptCur/bin/perch-open.sh" } else { (ConvertTo-WslPath $Root) + '/bin/perch-open.sh' }
    # Stdout is captured (it is the one-line link for --code); stderr goes straight to the console.
    $out = & wsl.exe @WslD -e bash -lc "'$script' $flag"
    return [pscustomobject]@{ Code = $LASTEXITCODE; Lines = @($out | ForEach-Object { "$_".Trim() } | Where-Object { $_ }) }
}

# Has `minerva setup` been run? It saves <home>/minerva.toml; the home is MINERVA_HOME or ~/Minerva in WSL.
function Test-Setup {
    Set-WslEnv
    & wsl.exe @WslD -e bash -lc 'test -f "${MINERVA_HOME:-$HOME/Minerva}/minerva.toml"'
    return ($LASTEXITCODE -eq 0)
}

# The setup wizard (setup/ in the repo, built in WSL), run on the console: NOT output-captured, because
# the prompts are drawn and answered through it. bin/minerva-setup.sh says how to build it when missing
# (exit 3). A first-run call ($Strict false) goes on with the defaults then; `hoot init` does not.
function Invoke-Wizard([bool]$Strict) {
    Set-WslEnv
    $script = if ($Installed) { "$OptCur/bin/minerva-setup.sh" } else { (ConvertTo-WslPath $Root) + '/bin/minerva-setup.sh' }
    & wsl.exe @WslD -e bash -lc "bash '$script'"
    $code = $LASTEXITCODE
    if ($code -eq 0) { return }
    if ($code -eq 3 -and -not $Strict) { Warn 'the setup wizard is not built; going on with the defaults (hoot init once it is)'; return }
    if ($code -eq 130) { Die 'setup cancelled; run hoot init when you are ready' }
    Die "the setup wizard did not finish (exit $code; the message above says why)"
}

# Installed copies only: a once-a-day, notice-only check (the Linux side does the asking, signed and cached; this
# never installs anything and says nothing when it cannot reach the network).
function Show-UpdateNotice {
    if (-not $Installed -or $env:MINERVA_NO_UPDATE_CHECK) { return }
    try {
        Set-WslEnv
        $line = @(& wsl.exe @WslD -e "$OptCur/bin/minerva" check-update --as hoot)
        if ($LASTEXITCODE -eq 0 -and $line.Count -gt 0 -and "$($line[0])".Trim()) { Warn "$($line[0])".Trim() }
    } catch { }
}

# The installed bundle's version (what `current` points at), or $null when WSL cannot say.
function Get-BundleVersion {
    $v = @(& wsl.exe @WslD -e cat "$OptCur/VERSION")
    if ($LASTEXITCODE -ne 0 -or -not $v) { return $null }
    $t = "$($v[0])".Trim()
    if ($t) { return $t } else { return $null }
}

# Record which bundle version these hoot files belong to (install.json keeps distro and prefix as they were).
function Set-InstalledVersion([string]$version) {
    $o = Get-Content -Raw $Marker | ConvertFrom-Json
    $o.version = $version
    [System.IO.File]::WriteAllText($Marker, ($o | ConvertTo-Json -Compress), (New-Object System.Text.UTF8Encoding($false)))
}

# Copy the bundle's hoot.ps1 over this one. Returns $true when it did.
function Sync-HootFiles {
    $unc = @(& wsl.exe @WslD -e wslpath -w "$OptCur/windows")
    if ($LASTEXITCODE -ne 0 -or -not $unc) { Warn 'could not find the new hoot files; hoot stays as it was'; return $false }
    $copied = $false
    # hoot.cmd is deliberately NOT copied: it is a fixed one-liner that ends itself (`& exit /b`), so cmd never re-reads the file
    # while it runs, and it never changes between versions. Only the script is refreshed. (An install made before the
    # one-liner was fixed keeps its old hoot.cmd until minerva-install.exe is run again.)
    $from = Join-Path "$($unc[0])".Trim() 'hoot.ps1'
    if (Test-Path $from) { Copy-Item -LiteralPath $from -Destination (Join-Path $PSScriptRoot 'hoot.ps1') -Force; $copied = $true }
    if (-not $copied) { Warn 'the bundle has no hoot.ps1; hoot stays as it was'; return $false }
    $v = Get-BundleVersion
    if ($v) { Set-InstalledVersion $v }
    Ok 'hoot refreshed'
    return $true
}

# Installed copies, every launch: when the bundle is not the version these hoot files came from, refresh them. The
# copy happens at the end of the run (the script is read already), and the person runs the command again.
function Update-SelfIfStale {
    if (-not $Installed) { return }
    $bundle = Get-BundleVersion
    if (-not $bundle -or $bundle -eq [string]$Inst.version) { return }
    Say "the installed Minerva is $bundle and this hoot is from $($Inst.version); refreshing hoot"
    if (Sync-HootFiles) {
        Say 'hoot is up to date now: run your command again'
        exit 0
    }
}

# hoot keys ..., hoot jev-setup: run the Linux command. A key file named by a Windows path is mapped into WSL first.
function Invoke-MinervaVerb([string[]]$words) {
    Set-WslEnv
    $mapped = @()
    foreach ($w in $words) {
        if ($w -ne '-' -and -not $w.StartsWith('-') -and $w -match '[\\/]' -and (Test-Path -LiteralPath $w -PathType Leaf)) { $mapped += (ConvertTo-WslPath (Resolve-Path -LiteralPath $w).ProviderPath) }
        else { $mapped += $w }
    }
    & wsl.exe @WslD -e "$OptCur/bin/minerva" @mapped
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

# Starts or reuses the server, gets a one-time link, and opens it. Returns nothing.
function Open-Web {
    if (-not (Test-Setup)) {
        Say 'first run: a few questions first (hoot init asks them again any time)'
        Invoke-Wizard $false
    }
    Say 'starting Minerva'
    $r = Invoke-PerchOpen '--code'
    if ($r.Code -ne 0) { Die 'the Minerva server did not start (the message above says why)' }
    $url = $r.Lines | Select-Object -Last 1
    if ($url -notmatch '^http://127\.0\.0\.1:\d+/#code=[A-Za-z0-9_-]+$') { Die 'the Minerva server answered with something that is not a link' }
    if ($NoOpen) { Write-Host $url; return }
    Start-Process $url
    Ok 'opened Minerva in your browser'
    Show-UpdateNotice
}

function Invoke-PerchVerb([string]$flag) {
    $r = Invoke-PerchOpen $flag
    foreach ($l in $r.Lines) { Write-Host "  $l" }
    if ($r.Code -ne 0) { exit $r.Code }
    if ($flag -eq '--status') { Show-UpdateNotice }
}

# Installed copies have no llama.cpp\ or models.ini: those verbs need a checkout.
$cmdLower = $Command.ToLower()
if ($Installed -and ($cmdLower -in 'serve', 'up', 'models', 'get', 'setup', 'edit', 'which' -or ($cmdLower -in 'stop', 'status' -and -not ($Rest -and $Rest[0] -eq 'web')))) {
    Die "'$Command' needs a Minerva checkout (llama-server lives there). Installed: hoot, hoot init, hoot status web, hoot stop web, hoot update, hoot keys, hoot jev-setup"
}
if ($cmdLower -ne 'update') { Update-SelfIfStale }

switch ($Command.ToLower()) {

    { $_ -in '', 'web', 'webui' } {
        # 'webui' is the old name: it used to say "moved"; it now opens Minerva too.
        Open-Web
    }

    'update' {
        if (-not $Installed) { Die 'hoot update is for an installed Minerva (minerva-install.exe). From a checkout: git pull.' }
        Set-WslEnv
        $flags = @($Rest | Where-Object { $_ })
        if ($Rollback) { $flags += '--rollback' }
        if ($Check) { $flags += '--check' }
        & wsl.exe @WslD -e "$OptCur/bin/minerva" update @flags
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        if (-not $Check -and -not ($flags -contains '--check')) { [void](Sync-HootFiles) }
    }

    'keys' {
        if (-not $Installed) { Die 'hoot keys is for an installed Minerva (minerva-install.exe)' }
        Invoke-MinervaVerb (@('keys') + @($Rest | Where-Object { $_ }))
    }

    'jev-setup' {
        if (-not $Installed) { Die 'hoot jev-setup is for an installed Minerva (minerva-install.exe)' }
        Invoke-MinervaVerb (@('jev-setup') + @($Rest | Where-Object { $_ }))
    }

    'version' {
        if (-not $Installed) { Die 'hoot version is for an installed Minerva (minerva-install.exe)' }
        Write-Host "hoot files  $($Inst.version)"
        Write-Host "bundle      $(Get-BundleVersion)"
    }

    'init' {
        # Not 'setup': that one is llama's (it fetches the Windows build).
        Invoke-Wizard $true
    }

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
        if ($Rest -and $Rest[0] -eq 'web') { Invoke-PerchVerb '--stop'; break }
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
        if ($Rest -and $Rest[0] -eq 'web') { Invoke-PerchVerb '--status'; break }
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
        # setup (plugins, providers) is done there.
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

    { $_ -in 'shim', 'aoide', 'theme', 'bench', 'jev' } {
        # Removed 2026-09-26, when hoot became the model-server launcher
        # only. Kept as a catch for old muscle memory, the way 'jev' was.
        Banner
        switch ($_) {
            'bench' { Die 'bench moved: llama.cpp\llama-bench.exe directly, or the bench loop in WSL' }
            default { Die "$_ moved: eidolon, Aoide and jev run in WSL now -- start them there" }
        }
    }

    default {
        Banner
        Box 'commands' @(
            'web                   open Minerva (same as bare hoot)'
            'init                  first-run wizard: home, model, roots'
            'stop web              stop the Minerva server (chats are kept)'
            'status web            is the Minerva server up, and its chats'
            'serve                 start the llama.cpp router (foreground)'
            'up                    start the router in the background'
            'stop                  stop the router'
            'status                is it up + models served'
            'models                list models (served or configured)'
            'get <quant>           fetch a Qwen3.8-27B quant, e.g. UD-Q5_K_M'
            'setup [-Tag] [-Force] fetch upstream llama.cpp (win-vulkan-x64)'
            'edit                  edit models.ini'
            'which                 show binaries/config in use'
            'help                  this list'
            'update                installed copies: install the newest signed release'
            'keys <verb>           installed copies: list/inspect/add/remove release signing keys'
            'jev-setup             installed copies: the optional jev chooser (asks first)'
        )
        Write-Host ''
        Write-Host "   llama api  $Base/v1   (Minerva itself runs in WSL; hoot web opens it)" -ForegroundColor DarkGray
        Write-Host ''
    }
}
