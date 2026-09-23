#requires -Version 7.0
param(
    [Parameter(Mandatory=$true)][string]$TaskFile,
    [Parameter(Mandatory=$true)][ValidatePattern('^[a-zA-Z0-9_-]+$')][string]$RunName,
    [ValidateRange(1,86400)][int]$TimeoutSeconds = 480
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$outDir = Join-Path $repo 'logs\agents'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$stdout = Join-Path $outDir "$RunName.stdout.log"
$stderr = Join-Path $outDir "$RunName.stderr.log"
$status = Join-Path $outDir "$RunName.status.json"
$info = [System.Diagnostics.ProcessStartInfo]::new()
$agentExe = @('release', 'debug') | ForEach-Object { Join-Path $repo "eidolon\target\$_\eidolon.exe" } | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $agentExe) { throw 'No built Eidolon executable found.' }
$info.FileName = $agentExe
$info.WorkingDirectory = $repo
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$info.RedirectStandardOutput = $true
$info.RedirectStandardError = $true
$gitBashDir = 'C:\Program Files\Git\bin'
if (Test-Path -LiteralPath (Join-Path $gitBashDir 'bash.exe')) { $info.Environment['PATH'] = $gitBashDir + ';' + $env:PATH }
foreach ($arg in @('run','--cwd',$repo,'--model','ollama:deepseek-v4.1-flash','--no-fallback',(Get-Content -Raw -LiteralPath $TaskFile))) { $info.ArgumentList.Add($arg) }
$process = [System.Diagnostics.Process]::new()
$process.StartInfo = $info
if ((Test-Path -LiteralPath $stdout) -or (Test-Path -LiteralPath $stderr)) { throw 'RunName already exists; choose a unique name to preserve evidence.' }
$started = [DateTime]::UtcNow
$process.Start() | Out-Null
$outStream = [System.IO.FileStream]::new($stdout, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite, 1)
$errStream = [System.IO.FileStream]::new($stderr, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite, 1)
$outCopy = $process.StandardOutput.BaseStream.CopyToAsync($outStream)
$errCopy = $process.StandardError.BaseStream.CopyToAsync($errStream)
$timedOut = $false
try {
    do {
        $elapsed = [int]([DateTime]::UtcNow - $started).TotalSeconds
        @{ pid=$process.Id; model='ollama:deepseek-v4.1-flash'; elapsedSeconds=$elapsed; timeoutSeconds=$TimeoutSeconds; state='running'; checkedUtc=[DateTime]::UtcNow.ToString('o') } | ConvertTo-Json | Set-Content -LiteralPath $status
        Write-Output "$RunName pid=$($process.Id) elapsed=${elapsed}s stdout=$($outStream.Length) stderr=$($errStream.Length)"
        if ($elapsed -ge $TimeoutSeconds) { $timedOut = $true; $process.Kill($true); break }
        $remainingMs = [Math]::Max(1, [Math]::Min(10000, [int](1000 * ($TimeoutSeconds - ([DateTime]::UtcNow - $started).TotalSeconds))))
    } while (-not $process.WaitForExit($remainingMs))
    $process.WaitForExit()
    # A descendant can inherit a pipe; never wait indefinitely for its EOF.
    [void]$outCopy.Wait(2000)
    [void]$errCopy.Wait(2000)
} finally {
    if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit() }
    $outStream.Dispose()
    $errStream.Dispose()
}
$state = if ($timedOut) { 'timed_out' } elseif ($process.ExitCode -eq 0) { 'exited' } else { 'failed' }
@{ pid=$process.Id; model='ollama:deepseek-v4.1-flash'; state=$state; exitCode=$process.ExitCode; elapsedSeconds=[int]([DateTime]::UtcNow-$started).TotalSeconds; stdout=$stdout; stderr=$stderr } | ConvertTo-Json | Set-Content -LiteralPath $status
Get-Content -LiteralPath $status
Get-Content -LiteralPath $stderr -Tail 30
Get-Content -LiteralPath $stdout -Tail 80
if ($timedOut) { exit 124 }
exit $process.ExitCode
