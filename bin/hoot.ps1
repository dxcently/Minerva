# hoot -- launcher and CLI control for Minerva
#
# Strictly 7-bit ASCII on purpose: PowerShell 5.1 reads .ps1 as ANSI unless the
# file carries a BOM, so box-drawing glyphs turn to mojibake. ASCII always works.
#
#   hoot serve [-Port N] [-Bind ADDR]   start llama-server (router mode)
#   hoot shim  [-ShimPort N]            start `eidolon serve` (the agent shim)
#   hoot webui [-WebuiPort N]           start Open WebUI through eidolon
#   hoot up                             serve + shim + webui backgrounded, then aoided
#   hoot stop                           stop serve + shim + webui (aoided stays up)
#   hoot aoide [start|stop|restart]     aoided, the WSL mesh node (default: status)
#   hoot status                         what is up, and what is loaded
#   hoot models                         list models from models.ini / the API
#   hoot get <quant>                    download a Qwen3.8-27B quant
#   hoot bench [-Model NAME]            benchmark a local model
#   hoot theme [NAME|--list|--restore]  eidolon TUI colours from a web UI scheme
#   hoot setup                          re-run the installer
#   hoot edit                           open models.ini
#   hoot which                          show which llama.cpp binaries are in use

[CmdletBinding()]
param(
    [Parameter(Position = 0)][string]$Command = 'help',
    [Parameter(Position = 1, ValueFromRemainingArguments = $true)][string[]]$Rest,
    [int]$Port = 8080,
    [int]$WebuiPort = 3000,
    # `eidolon serve`'s own default (crates/cli `[serve] bind`). Repeated
    # here rather than read out of config.toml because this launcher only
    # ever needs to *reach* the shim, and a wrong guess shows up as a dead
    # connection in the app rather than as a silently wrong write.
    [int]$ShimPort = 8085,
    [string]$Bind = '127.0.0.1',
    [string]$Model
)

$ErrorActionPreference = 'Stop'
$Root   = Split-Path $PSScriptRoot -Parent
$Bin    = Join-Path $Root 'llama.cpp'
$Ini    = Join-Path $Root 'models.ini'
$Models = Join-Path $Root 'models'
$Cache  = Join-Path $Root '.cache'
$Base   = "http://${Bind}:${Port}"
$WebUI  = "http://${Bind}:${WebuiPort}"
# The OpenAI-compatible shim eidolon serves: one agent session per chat,
# registered in Open WebUI beside the router as a second connection (see
# Set-WebuiEnv, and docs/design/shim.md "Registration: the contract with
# the launcher"). Not the same thing as $Base -- the router does
# inference, the shim runs an agent with tools behind the policy gate.
$Shim   = "http://${Bind}:${ShimPort}"
$Logs   = Join-Path $Root 'logs'
# eidolon owns jev now -- it is one of the directories under here, started
# and health-checked the way every extension service is (eidolon ext start),
# not spawned by this launcher. `hoot setup` points eidolon at this
# directory instead of copying it in, through `eidolon ext dir` (see
# Get-EidolonExe and the 'setup'/'up' cases below for why a real subcommand
# replaced what used to be a hand-rolled config.toml editor here).
$Extensions = Join-Path $Root 'extensions'
# Unset means wsl.exe's default distro.
$Distro = $env:AOIDE_DISTRO

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
function Get-WebUI {
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and ($_.CommandLine -like '*open-webui*' -or $_.CommandLine -like '*open_webui*') -and $_.Name -like '*python*' }
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

function Get-Owui {
    # `uv tool install` drops a shim in ~\.local\bin, which is on the
    # interactive PATH but not necessarily on a service's or a subshell's.
    # Trusting PATH alone made 'hoot up' quietly start the router only, so
    # look there too before giving up.
    $c = Get-Command open-webui -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    $shim = Join-Path $env:USERPROFILE '.local\bin\open-webui.exe'
    if (Test-Path $shim) { return $shim }
    return $null
}

# aoided is a systemd user unit inside WSL with linger on, so hoot drives the
# unit, never the process.
function Invoke-AoidedUnit([string]$verb) {
    # Native stderr under 'Stop' throws in PowerShell 5.1.
    $ErrorActionPreference = 'Continue'
    $wslArgs = @()
    if ($Distro) { $wslArgs += '-d', $Distro }
    $wslArgs += '--exec', 'systemctl', '--user', $verb, 'aoided'
    $out = & wsl.exe @wslArgs 2>$null
    [pscustomobject]@{ Ok = ($LASTEXITCODE -eq 0); Out = (@($out) -join ' ').Trim() }
}

function Get-EidolonConfigDir {
    # Whatever `dirs::config_dir()` answers, because that is what eidolon
    # itself uses to place config.toml and serve.token: %APPDATA%\eidolon
    # on Windows, $XDG_CONFIG_HOME (else ~/.config) elsewhere. Derived
    # rather than hardcoded so the two halves cannot drift into naming
    # different files -- the exact shape S11 is open about one layer down.
    if ($env:APPDATA) { return (Join-Path $env:APPDATA 'eidolon') }
    $xdg = $env:XDG_CONFIG_HOME
    if (-not $xdg) { $xdg = Join-Path $HOME '.config' }
    return (Join-Path $xdg 'eidolon')
}

function Get-ShimTokenPath { Join-Path (Get-EidolonConfigDir) 'serve.token' }

function Get-ShimToken {
    # The bearer the shim checks on every /v1 request. `eidolon serve`
    # writes it once, owner-only, on first run; this reads it and hands it
    # to Open WebUI's ENVIRONMENT (docs/design/shim.md section 6) -- never
    # an argument, never echoed, never in a Box on the screen. $null when
    # the shim has not run yet, which is a real state on a fresh box and
    # is reported as one.
    $p = Get-ShimTokenPath
    if (-not (Test-Path $p)) { return $null }
    $t = ([System.IO.File]::ReadAllText($p)).Trim()
    if (-not $t) { return $null }
    return $t
}

function Test-IsHootShim($exe, $cmdline) {
    # Only a process that looks EXACTLY like what 'shim' and 'up' below
    # start: this repo's own resolved binary, the bare `serve` verb, and
    # neither --config nor --bind, which this launcher never passes.
    #
    # Deliberately narrow, and the reason is worth keeping. The first
    # version of this matched any eidolon process whose command line
    # contained `serve`, and on this box it immediately matched a second
    # one: an agent's isolated trial running target\debug\eidolon.exe
    # with its own --config and --bind 127.0.0.1:18787, in a scratch
    # directory. `hoot stop` would have killed somebody else's live work
    # -- the same mistake, in a different medium, as deleting a git
    # worktree you did not create (Decisions.md, 2026-09-19, "a worktree
    # is named for the work"). A process this launcher cannot prove it
    # started is a process it leaves alone.
    #
    # `serve` as a whole word, too: `eidolon sessions` and `eidolon web`
    # are other long-lived eidolon processes and neither is this one.
    if (-not $cmdline) { return $false }
    $mine = Get-EidolonExe
    if (-not $mine) { return $false }
    if (-not $exe -or ($exe -ne $mine)) { return $false }
    if ($cmdline -notmatch '(?<!\S)serve(?!\S)') { return $false }
    if ($cmdline -match '(?<!\S)--(config|bind)(?!\S)') { return $false }
    return $true
}

function Get-ShimPid {
    # By command line, not by port: what `hoot stop` must stop is the
    # process this launcher started, and a port can be held by anything.
    # Win32_Process is where a command line lives on Windows; PowerShell 6+
    # puts one straight on Get-Process, which is what a Linux box would
    # use -- both are tried, so the eidolon half of this launcher stays
    # portable even though the router half cannot be (Decisions.md,
    # 2026-09-19, "the launcher cannot be made POSIX").
    try {
        $p = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
               Where-Object { $_.Name -like 'eidolon*' -and (Test-IsHootShim $_.ExecutablePath $_.CommandLine) } |
               ForEach-Object { $_.ProcessId })
        if ($p.Count) { return $p }
    } catch { }
    @(Get-Process eidolon -ErrorAction SilentlyContinue |
      Where-Object { Test-IsHootShim $_.Path $_.CommandLine } |
      ForEach-Object { $_.Id })
}

function Write-Loader($src, $dst) {
    $js = [System.IO.File]::ReadAllText($src)
    $js = $js -replace "var SHIM_ORIGIN = '[^']*';\s*/\* hoot-shim-origin \*/", "var SHIM_ORIGIN = '$Shim';  /* hoot-shim-origin */"
    $js = $js -replace "var ROUTER_ORIGIN = '[^']*';\s*/\* hoot-router-origin \*/", "var ROUTER_ORIGIN = '$Base';  /* hoot-router-origin */"
    [System.IO.File]::WriteAllText($dst, $js)
}

function Set-WebuiEnv {
    $token = Get-ShimToken
    if (-not $token) { Die "no Eidolon token at $(Get-ShimTokenPath); start the shim before WebUI" }
    $env:OPENAI_API_BASE_URL  = "$Shim/v1"
    $env:OPENAI_API_KEY       = $token
    $env:OPENAI_API_BASE_URLS = "$Shim/v1"
    $env:OPENAI_API_KEYS      = $token
    $env:OPENAI_API_CONFIGS   = '{"0":{"enable":true,"tags":["agent"],"headers":{"X-Eidolon-Chat-Id":"{{CHAT_ID}}","X-Eidolon-Task":"{{TASK}}","X-Eidolon-Message-Id":"{{MESSAGE_ID}}","X-Eidolon-User-Message-Id":"{{USER_MESSAGE_ID}}","X-Eidolon-Parent-Id":"{{USER_MESSAGE_PARENT_ID}}"}}}'
    $env:TASK_MODEL_EXTERNAL  = 'hoot:gpt-oss-20b'
    $env:ENABLE_OLLAMA_API   = 'False'
    # The chat-to-session join (Decisions.md, 2026-09-18 "shim design"
    # Amendment 3) reads `{{CHAT_ID}}` off a per-connection header, not this
    # toggle -- that is scoped to the eidolon connection instead of forwarding
    # user identity to every OpenAI endpoint configured. Kept on for
    # attribution, and as a fallback the shim reads if its own header is
    # somehow missing; the mapping does not depend on it.
    $env:ENABLE_FORWARD_USER_INFO_HEADERS = 'True'
    $env:WEBUI_NAME          = 'Minerva'
    # Open WebUI appends ' (Open WebUI)' to any custom name. Its LICENSE
    # permits removing that branding only for deployments under fifty end
    # users in a rolling 30 days -- a single-operator local box, which this
    # is. Ship it wider than that and this line must come back out.
    $env:WEBUI_NAME_SUFFIX   = 'false'
    # WEBUI_AUTH=False alone deadlocks: onboarding still wants an initial admin
    # while signup is off, so nothing can create the one account it insists on
    # and every API call answers 401. Leaving signup open lets the first visit
    # make that account; auth stays off afterwards, and this is bound to
    # loopback anyway.
    $env:WEBUI_AUTH          = 'False'
    $env:ENABLE_SIGNUP       = 'True'
    $env:DEFAULT_USER_ROLE   = 'admin'
    $env:DATA_DIR            = (Join-Path $Root '.webui')
    $env:ANONYMIZED_TELEMETRY = 'False'
    New-Item -ItemType Directory -Force -Path $env:DATA_DIR | Out-Null

    # Banner inside the web UI. One line of plain markdown: no ASCII art --
    # it reflowed into broken figures -- and no blanket privacy claim, since
    # a configured cloud model does leave the box.
    $art = 'Make it, Break it, Hack it.'

    $banner = @(@{
        id          = 'hoot'
        type        = 'info'
        title       = 'Minerva'
        content     = $art
        dismissible = $true
        timestamp   = [int][double]::Parse((Get-Date -UFormat %s))
    })
    # -InputObject with @(): a single-element array piped to ConvertTo-Json
    # unwraps to a bare object, and 0.11.4 iterates that as a list of its keys.
    $env:WEBUI_BANNERS = (ConvertTo-Json -Compress -Depth 4 -InputObject @($banner))

    # Club skin. Open WebUI requests /static/custom.css on every page load;
    # the file ships empty, so copying ours in is the supported theming path.
    # Re-copied on every launch because a 'uv tool upgrade open-webui' blanks
    # it again -- the repo copy under webui\ is the source of truth.
    $skin = Join-Path $Root 'webui\custom.css'
    if (Test-Path $skin) {
        # uv puts the real package under APPDATA\uv\tools, and the thing on
        # PATH is only a shim in .local\bin -- so resolve the tool dir
        # directly rather than walking up from Get-Command.
        $pkg = Join-Path $env:APPDATA 'uv\tools\open-webui\Lib\site-packages\open_webui'
        $env:MINERVA_EIDOLON_ORIGIN = $Shim
        $env:MINERVA_WORKSPACE = $Root
        $env:MINERVA_EIDOLON_TOKEN_FILE = Get-ShimTokenPath
        Copy-Item (Join-Path $Root 'webui\minerva_credentials.py') (Join-Path $pkg 'routers\minerva_credentials.py') -Force
        Copy-Item (Join-Path $Root 'webui\minerva_workspace.py') (Join-Path $pkg 'routers\minerva_workspace.py') -Force
        Copy-Item (Join-Path $Root 'webui\minerva_catalog.py') (Join-Path $pkg 'routers\minerva_catalog.py') -Force
        $mainFile = Join-Path $pkg 'main.py'
        $mainText = [System.IO.File]::ReadAllText($mainFile)
        if (-not $mainText.Contains('app.include_router(minerva_credentials.router)')) {
            $anchor = "app.mount('/static',"
            if (-not $mainText.Contains($anchor)) { throw 'Open WebUI static mount changed; credential router was not installed.' }
            $mainText = $mainText.Replace($anchor, "from open_webui.routers import minerva_credentials`napp.include_router(minerva_credentials.router)`n`n$anchor")
            [System.IO.File]::WriteAllText($mainFile, $mainText)
        }
        if (-not $mainText.Contains('app.include_router(minerva_workspace.router)')) {
            $mainText = $mainText.Replace('app.include_router(minerva_credentials.router)', "app.include_router(minerva_credentials.router)`nfrom open_webui.routers import minerva_workspace`napp.include_router(minerva_workspace.router)")
            [System.IO.File]::WriteAllText($mainFile, $mainText)
        }
        if (-not $mainText.Contains('minerva_catalog.install(app)')) {
            $mainText = $mainText.Replace('app.include_router(minerva_workspace.router)', "app.include_router(minerva_workspace.router)`nfrom open_webui.routers import minerva_catalog`nminerva_catalog.install(app)")
            [System.IO.File]::WriteAllText($mainFile, $mainText)
        }
        # The owl, in every brand slot Open WebUI reads. Same LICENSE note as
        # WEBUI_NAME_SUFFIX above: under-fifty-user deployments only.
        $brand = Join-Path $Root 'webui\brand'
        foreach ($d in @('static', 'frontend\static')) {
            $dst = Join-Path $pkg $d
            if (Test-Path $dst) {
                Copy-Item $skin (Join-Path $dst 'custom.css') -Force
                foreach ($asset in @('workspace.js', 'workspace.css', 'graph-adapter.js')) {
                    Copy-Item (Join-Path $Root "webui\$asset") (Join-Path $dst $asset) -Force
                }
                Copy-Item (Join-Path $Root 'eidolon\crates\web\assets\js\jev-flow.js') (Join-Path $dst 'jev-flow.js') -Force
                Copy-Item (Join-Path $Root 'eidolon\crates\web\assets\styles\jev-flow.css') (Join-Path $dst 'jev-flow.css') -Force
                if (Test-Path $brand) { Copy-Item (Join-Path $brand '*') $dst -Force }
                # The runtime icon swapper, and the three eidolon nav rows.
                # index.html loads /static/loader.js deferred and ships it
                # empty -- the JS twin of custom.css. Written through
                # Write-Loader rather than copied, because two facts in it
                # are this launcher's to fill in; see there.
                $ldr = Join-Path $Root 'webui\loader.js'
                if (Test-Path $ldr) { Write-Loader $ldr (Join-Path $dst 'loader.js') }
            }
        }
        # env.py hard-codes the name suffix; patch it to honour the variable.
        $envpy = Join-Path $pkg 'env.py'
        if (Test-Path $envpy) {
            $py = [System.IO.File]::ReadAllText($envpy)
            $was = "if WEBUI_NAME != 'Open WebUI':"
            $now = "if WEBUI_NAME != 'Open WebUI' and os.getenv('WEBUI_NAME_SUFFIX', 'true').lower() != 'false':"
            if ($py.Contains($was)) { [System.IO.File]::WriteAllText($envpy, $py.Replace($was, $now)) }
        }
        # Tab icon: point the shipped index.html at the svg owl. Idempotent,
        # and re-applied here because an upgrade restores the stock line.
        $idx = Join-Path $pkg 'frontend\index.html'
        if (Test-Path $idx) {
            $html = [System.IO.File]::ReadAllText($idx)
            # Refresh supported skin hooks after an upgrade or a skin edit.
            foreach ($asset in @('loader.js', 'custom.css')) {
                $assetHash = (Get-FileHash (Join-Path $Root "webui\$asset") -Algorithm SHA256).Hash.Substring(0,12)
                $assetPattern = '/static/' + [regex]::Escape($asset) + '(?:\?v=[^" ]*)?'
                $html = [regex]::Replace($html, $assetPattern, "/static/${asset}?v=$assetHash")
            }

            $stock = '<link rel="icon" type="image/png" href="/static/favicon.png"'
            $owlLink = '<link rel="icon" type="image/svg+xml" href="/static/favicon.svg"'
            if ($html.Contains($stock)) {
                [System.IO.File]::WriteAllText($idx, $html.Replace($stock, $owlLink))
            }
        }
    }
}

function Get-EidolonExe {
    # This repo's own build wins, unconditionally -- PATH is not consulted.
    # That is the OPPOSITE rule from Get-Owui above, on purpose: open-webui
    # is a third-party tool this repo never builds, so PATH (or the uv shim
    # fallback) is the only place it could ever be. eidolon is not that --
    # it is this repo's own binary, built by the 'cargo build --release'
    # the failure message below already points the operator at, straight
    # into eidolon\target right here. Trusting PATH first for it would
    # repeat exactly the hazard 'serve' below already refuses for
    # llama-server.exe ("Absolute path on purpose: a bare llama-server
    # resolves to the winget build..."): a same-named binary elsewhere on
    # PATH -- a stale `cargo install`, a copy left on PATH by an unrelated
    # project -- would silently win over the copy this repo just built, and
    # there would be no way to tell from here which one answered. So: same
    # rule for both binaries this repo builds itself, not two different
    # ones with no comment saying so. Release preferred over debug.
    # Relative to $Root (derived from $PSScriptRoot), not the caller's cwd,
    # so this works the same whichever directory `hoot` was run from.
    #
    # Existence only -- not a capability probe. A release binary that
    # predates a subcommand is a real state this repo can be in (it was,
    # the day this was written: target\release here was built before `ext
    # dir` existed and target\debug was not), and the call site below that
    # actually runs the binary says so itself, pointing at a rebuild,
    # rather than this function silently trying the next candidate and
    # hiding which one is stale.
    foreach ($p in 'release', 'debug') {
        $candidate = Join-Path $Root "eidolon\target\$p\eidolon.exe"
        if (Test-Path $candidate) { return $candidate }
    }
    return $null
}

function Test-ExtensionsDirLooksRight {
    # Advisory only -- a substring scan, not a TOML parse, and that is fine
    # here in a way it would not be for a write: a false negative costs one
    # extra sentence telling the operator to run `hoot setup`; a false
    # positive stays quiet. Neither writes a byte. `up` must never edit
    # config.toml -- mutating the operator's file on every launch is churn
    # on a value that changes only when the repo moves, and `up` is the hot
    # path where a bad write would do the most damage -- so this is the one
    # place in this file allowed to be sloppy about what is actually in
    # that file. `hoot setup` is the one place allowed to write it, and it
    # does that through eidolon's own parser (`eidolon ext dir`), never by
    # scanning text.
    $cfg = Join-Path $env:APPDATA 'eidolon\config.toml'
    if (-not (Test-Path $cfg)) { return $false }
    $want = $Extensions -replace '\\', '/'
    (Get-Content -Raw $cfg) -match [regex]::Escape($want)
}

switch ($Command.ToLower()) {

    'serve' {
        Banner
        if (Get-Server) { Die 'already running (hoot stop, or hoot status)' }
        if (-not (Test-Path (Join-Path $Bin 'llama-server.exe'))) { Die 'no binaries; run: hoot setup' }
        if (-not (Test-Path $Ini)) { Die "missing $Ini" }
        New-Item -ItemType Directory -Force -Path $Cache | Out-Null
        # Isolated cache so the router lists only OUR models, not stray
        # half-cached entries left by the winget llama.cpp install.
        $env:LLAMA_CACHE = $Cache
        Box 'router' @("api     $Base/v1", "webui   $Base", "config  $Ini", '', 'ctrl-c to stop')
        Write-Host ''
        # Absolute path on purpose: a bare llama-server resolves to the winget
        # build, which cannot load the P* ternary quants.
        & (Join-Path $Bin 'llama-server.exe') `
            --models-preset $Ini --models-max 2 --host $Bind --port $Port @Rest
    }

    'shim' {
        # `eidolon serve`: the OpenAI-compatible endpoint that summons one
        # eidolon agent per Open WebUI chat. Foreground, like 'serve'
        # above; 'up' runs the same thing backgrounded.
        Banner
        if (Get-ShimPid) { Die 'already running (hoot stop, or hoot status)' }
        $eidolon = Get-EidolonExe
        if (-not $eidolon) { Die 'no eidolon binary; build it: cd eidolon; cargo build --release' }
        # --cwd explicitly: every chat runs its tools here until the
        # operator moves one with /cwd, and `[serve] cwd`'s fallback is the
        # *launch* directory -- which is wherever `hoot` happened to be
        # typed. $Root is derived from $PSScriptRoot, so this is the same
        # directory whichever one that was.
        Box 'eidolon shim' @("api     $Shim/v1", "webui   http://127.0.0.1:3000", "cwd     $Root", "token   $(Get-ShimTokenPath)", '', 'ctrl-c to stop')
        Write-Host ''
        & $eidolon serve --cwd $Root @Rest
    }

    'jev' {
        # eidolon owns the choosers now: a service under extensions\jev,
        # started and health-checked the way every extension is, not a
        # standalone sidecar this launcher spawns and tracks by port. Kept
        # as a verb, rather than removed outright, only to catch the old
        # muscle memory and point it at the replacement.
        Banner
        Die 'jev moved: eidolon ext start jev  (eidolon ext list to see it)'
    }

    'webui' {
        Banner
        $owui = Get-Owui
        if (-not $owui) { Die 'open-webui not installed. run:  uv tool install open-webui --python 3.12' }
        if (-not (Test-Api "$Base/v1/models")) { Warn "router is down -- start it with 'hoot serve' (or use 'hoot up')" }
        # Said before Set-WebuiEnv runs, because that is the moment the
        # registration is decided: the connection is seeded from the token
        # file, and a shim that has never run has not written one. 'up'
        # starts it first for exactly this reason.
        if (-not (Get-ShimPid)) { Warn "eidolon shim is down -- start it with 'hoot shim' (or use 'hoot up')" }
        Set-WebuiEnv
        Box 'open webui' @("ui       $WebUI", "router   $Base/v1", "agent    $Shim/v1", "data     $($env:DATA_DIR)") 'Magenta'
        Write-Host ''
        & $owui serve --host $Bind --port $WebuiPort @Rest
    }

    'up' {
        Banner
        if (-not (Get-Server)) {
            Say 'starting router'
            New-Item -ItemType Directory -Force -Path $Cache | Out-Null
            $env:LLAMA_CACHE = $Cache
            Start-Process -FilePath (Join-Path $Bin 'llama-server.exe') `
                -ArgumentList '--models-preset', $Ini, '--models-max', '2', '--host', $Bind, '--port', $Port `
                -WindowStyle Hidden
            if (-not (Wait-Api "$Base/v1/models" 600 'router')) { Die 'router failed to start' }
        } else { Ok 'router already up' }

        # Advise, never write: see Test-ExtensionsDirLooksRight for why a
        # loose check is the right amount of care for this call site.
        if (-not (Test-ExtensionsDirLooksRight)) {
            Warn 'extensions_dir looks unset or stale -- run: hoot setup'
        }

        # Before Open WebUI, not after: Set-WebuiEnv reads the shim's token
        # file to register the connection, and on a box that has never run
        # `eidolon serve` that file does not exist until it has. Starting
        # the shim second would register the router alone and need a second
        # launch to fix.
        if (Get-ShimPid) { Ok 'eidolon shim already up' }
        else {
            $eidolon = Get-EidolonExe
            if (-not $eidolon) {
                Warn 'no eidolon binary -- no agent in the model picker'
                Warn 'build it:  cd eidolon; cargo build --release'
            } else {
                Say 'starting eidolon shim'
                New-Item -ItemType Directory -Force -Path $Logs | Out-Null
                # Redirected rather than discarded: the shim's stderr is
                # where an extension that would not start says so, and
                # where the one misconfiguration that is otherwise silent
                # -- a task-shaped request arriving with no chat id --
                # announces itself. A hidden window would eat both.
                Start-Process -FilePath $eidolon `
                    -ArgumentList 'serve', '--cwd', $Root `
                    -WorkingDirectory $Root -WindowStyle Hidden `
                    -RedirectStandardOutput (Join-Path $Logs 'shim.out.log') `
                    -RedirectStandardError  (Join-Path $Logs 'shim.err.log')
                if (-not (Wait-Api "$Shim/api/ext" 90 'eidolon shim')) {
                    Warn "shim did not answer -- see $(Join-Path $Logs 'shim.err.log')"
                }
            }
        }

        $owui = Get-Owui
        if (-not $owui) {
            Warn 'open-webui not installed -- router only'
            Warn 'install:  uv tool install open-webui --python 3.12'
        } elseif (Get-WebUI) { Ok 'open webui already up' }
        else {
            Say 'starting open webui'
            Set-WebuiEnv
            Start-Process -FilePath $owui `
                -ArgumentList 'serve', '--host', $Bind, '--port', $WebuiPort -WindowStyle Hidden
            Wait-Api "$WebUI/health" 420 'open webui' | Out-Null
        }

        if ((Invoke-AoidedUnit 'is-active').Ok) { Ok 'aoided already up' }
        else {
            Say 'starting aoided'
            $r = Invoke-AoidedUnit 'start'
            if (-not $r.Ok) { Warn "aoided did not start -- hoot aoide for its state" }
        }
        Write-Host ''
        & $PSCommandPath status
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
        $w = @(Get-WebUI)
        foreach ($x in $w) { Stop-Process -Id $x.ProcessId -Force -ErrorAction SilentlyContinue }

        # The shim last: it is what an open chat is talking to, so killing
        # it before the app would turn a turn in flight into a red error
        # on a bubble instead of a stream the app simply stops reading.
        $s = @(Get-ShimPid)
        foreach ($id in $s) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }

        if (Get-Server) { Die 'llama-server still running' }
        if ($killed)    { Ok ("stopped router ({0} proc)" -f $killed) }
        if ($w.Count)   { Ok ("stopped open webui ({0} proc)" -f $w.Count) }
        if ($s.Count)   { Ok ("stopped eidolon shim ({0} proc)" -f $s.Count) }
        if (-not $killed -and -not $w.Count -and -not $s.Count) { Warn 'nothing was running' }
        # An extension service the shim started is eidolon's to stop, not
        # this launcher's: eidolon ext stop jev.
    }

    'status' {
        Banner
        $srvUp = Test-Api "$Base/v1/models"
        $wuiUp = Test-Api "$WebUI/health"
        $shmUp = Test-Api "$Shim/api/ext"
        $aoide = Invoke-AoidedUnit 'is-active'
        $where = if ($Distro) { "wsl $Distro" } else { 'wsl (default distro)' }
        Box 'status' @(
            ("{0} router      {1,-24} {2}" -f (Dot $srvUp), $Base,  $(if ($srvUp) { 'responding' } else { 'not responding' }))
            ("{0} eidolon     {1,-24} {2}" -f (Dot $shmUp), $Shim,  $(if ($shmUp) { 'responding' } else { 'not responding' }))
            ("{0} open webui  {1,-24} {2}" -f (Dot $wuiUp), $WebUI, $(if ($wuiUp) { 'responding' } else { 'not responding' }))
            ("{0} aoided      {1,-24} {2}" -f (Dot $aoide.Ok), $where, $(if ($aoide.Out) { $aoide.Out } else { 'unreachable' }))
        )
        if ($shmUp -and -not (Get-ShimToken)) {
            Warn "shim is up but $(Get-ShimTokenPath) is missing -- open webui cannot authenticate to it"
        }
        $served = @()
        if ($shmUp) {
            $t = Get-ShimToken
            if ($t) {
                try {
                    $served += (Invoke-RestMethod "$Shim/v1/models" -TimeoutSec 10 `
                                -Headers @{ Authorization = "Bearer $t" }).data |
                               ForEach-Object { "  * $(if ($_.name) { $_.name } else { $_.id })" }
                } catch { Warn 'shim process up but /v1/models not answering yet' }
            }
        }
        if ($served.Count) { Write-Host ''; Box 'served to open webui' $served }
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
        & (Join-Path $Root 'setup-bonsai2.ps1') -QwenQuant $Rest[0] -SkipBonsai
    }

    'bench' {
        Banner
        $target = if ($Model) { $Model } else { 'Qwen3.8-27B-UD-Q4_K_M' }
        $gguf = Get-ChildItem $Models -Recurse -Filter "*$target*.gguf" -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -notlike 'mmproj*' } | Select-Object -First 1
        if (-not $gguf) { Die "no model matching '$target' under $Models" }
        Say "benchmarking $($gguf.Name)"
        Write-Host ''
        & (Join-Path $Bin 'llama-bench.exe') -m $gguf.FullName -ngl 0 -ngl 99 -fa 1 -t 8 -p 128 -n 32 -r 1 @Rest
    }

    'setup' {
        Banner
        & (Join-Path $Root 'setup-bonsai2.ps1') @Rest
        # Point eidolon at the repo's own extensions (jev included) instead
        # of copying them in -- see the comment by $Extensions above for
        # why a copy breaks jev's service. This is the one place in this
        # file that writes config.toml, and it does that by shelling out to
        # eidolon's own parser rather than editing the file itself; the
        # write is exact, but finding a binary to run it with is best
        # effort, so a miss here is reported, not fatal.
        $eidolon = Get-EidolonExe
        if (-not $eidolon) {
            Warn 'no eidolon binary found (checked eidolon\target\release, eidolon\target\debug)'
            Warn 'extensions_dir not set -- build eidolon, then re-run: hoot setup'
            $onPath = Get-Command eidolon -ErrorAction SilentlyContinue
            if ($onPath) {
                # Get-EidolonExe deliberately does not use this -- see its
                # own comment -- but naming it here beats letting the
                # operator wonder why a working `eidolon` on PATH did not
                # satisfy this check.
                Warn "note: $($onPath.Source) is on PATH, but this launcher only trusts its own build"
            }
        } else {
            $out = & $eidolon ext dir ($Extensions -replace '\\', '/')
            if ($LASTEXITCODE -eq 0) {
                Ok "$out"
            } else {
                Warn "eidolon ext dir failed ($eidolon)"
                Warn 'if that said "unrecognized subcommand", the binary predates it -- rebuild: cd eidolon; cargo build --release'
            }
        }
    }

    'aoide' {
        Banner
        $verb = if ($Rest) { $Rest[0].ToLower() } else { 'status' }
        if ($verb -notin 'status', 'start', 'stop', 'restart') { Die "aoide: unknown '$verb' (start, stop, restart, status)" }
        if ($verb -ne 'status') {
            Say "$verb aoided"
            if (-not (Invoke-AoidedUnit $verb).Ok) { Warn "systemctl --user $verb aoided failed" }
        }
        $a = Invoke-AoidedUnit 'is-active'
        Box 'aoided' @(("{0} {1}" -f (Dot $a.Ok), $(if ($a.Out) { $a.Out } else { 'WSL or the unit is unreachable' })))
    }

    'theme' {
        $ErrorActionPreference = 'Continue'
        # The TUI runs natively here, so its ui.rn is the Windows one; /p has WSL translate the path.
        $env:EIDOLON_UI = Join-Path $env:APPDATA 'eidolon\ui.rn'
        $env:WSLENV = (@($env:WSLENV, 'EIDOLON_UI/p') | Where-Object { $_ }) -join ':'
        $wslArgs = @()
        if ($Distro) { $wslArgs += '-d', $Distro }
        $wslArgs += '--cd', $PSScriptRoot, '--exec', 'python3', './eidolon-theme'
        & wsl.exe @wslArgs @Rest
        exit $LASTEXITCODE
    }

    'edit' { Start-Process notepad.exe $Ini }

    'which' {
        Banner
        $other = Get-Command llama-server.exe -ErrorAction SilentlyContinue
        Box 'paths' @(
            "hoot uses   $Bin\llama-server.exe"
            "on PATH     $(if ($other) { $other.Source } else { '(none)' })"
            "models.ini  $Ini"
            "models      $Models"
            "cache       $Cache"
        )
        if ($other -and -not $other.Source.StartsWith($Bin)) {
            Write-Host ''
            Warn 'a different llama-server is on PATH; it cannot load the ternary quants'
        }
    }

    default {
        Banner
        Box 'commands' @(
            'serve                 start the llama.cpp router'
            'shim                  start eidolon serve (an agent per chat)'
            'webui                 start Open WebUI against both'
            'up                    all three in the background, then aoided'
            'stop                  stop all three (aoided stays up)'
            'aoide [start|stop]    the WSL mesh node; bare = status'
            'status                what is running + models served'
            'models                list models (served or configured)'
            'get <quant>           fetch a Qwen3.8-27B quant, e.g. UD-Q5_K_M'
            'bench [-Model NAME]   benchmark CPU vs Vulkan'
            'theme [name|--list]   eidolon TUI colours from a web UI scheme'
            'setup                 re-run the installer'
            'edit                  edit models.ini'
            'which                 show binaries/config in use'
        )
        Write-Host ''
        Write-Host "   api  $Base/v1   agent  $Shim/v1   ui  $WebUI" -ForegroundColor DarkGray
        Write-Host ''
    }
}
