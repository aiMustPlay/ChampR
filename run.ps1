param(
    [Parameter(Position = 0)]
    [ValidateSet('doctor', 'server', 'crawler', 'app', 'all', 'help')]
    [string]$Command = 'doctor',

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$RemainingArgs
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) {
    $env:Path = "$cargoBin;$env:Path"
}

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message" -ForegroundColor Cyan
    Add-Content -Path (Join-Path $RepoRoot '.cache\launcher.log') -Value ("==> " + $Message) -ErrorAction SilentlyContinue
}

function Write-Warn {
    param([string]$Message)
    Write-Host "!! $Message" -ForegroundColor Yellow
    Add-Content -Path (Join-Path $RepoRoot '.cache\launcher.log') -Value ("!! " + $Message) -ErrorAction SilentlyContinue
}

# Hidden launcher = nobody sees the console. Failures must surface on their own,
# so show a dialog box with the reason and the tail of the relevant log.
# ASCII-only comments here: PS 5.1 reads a BOM-less .ps1 as ANSI, CJK can break parsing.
function Show-Failure {
    param(
        [string]$Title,
        [string]$Message,
        [string]$LogPath
    )

    $details = ""
    if ($LogPath -and (Test-Path $LogPath)) {
        $tail = Get-Content $LogPath -Tail 20 -ErrorAction SilentlyContinue
        if ($tail) {
            $details = "`n`n--- $LogPath ---`n" + ($tail -join "`n")
        }
    }
    $full = $Message + $details
    Add-Content -Path (Join-Path $RepoRoot '.cache\launcher.log') -Value ("FAIL " + $Title + ": " + $Message) -ErrorAction SilentlyContinue

    try {
        Add-Type -AssemblyName System.Windows.Forms -ErrorAction Stop
        [System.Windows.Forms.MessageBox]::Show($full, $Title, 'OK', 'Error') | Out-Null
    }
    catch {
        Write-Warn $Message
    }
}

function Test-Cmd {
    param([string]$Name)
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

function Initialize-MsvcEnvironment {
    $vsDevCmd = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat'
    if (-not (Test-Path $vsDevCmd)) {
        return
    }

    $cmdLine = "`"$vsDevCmd`" -arch=amd64 -host_arch=amd64 && set"
    $output = cmd /c $cmdLine 2>$null
    foreach ($line in $output) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
        }
    }
}

function Set-NodeMirrors {
    $registry = $env:CHAMPR_NPM_REGISTRY
    if (-not $registry) {
        $registry = 'https://registry.npmmirror.com'
    }
    $env:COREPACK_NPM_REGISTRY = $registry
    $env:npm_config_registry = $registry

    $playwrightHost = $env:CHAMPR_PLAYWRIGHT_HOST
    if (-not $playwrightHost) {
        $playwrightHost = 'https://registry.npmmirror.com/-/binary/playwright'
    }
    $env:PLAYWRIGHT_DOWNLOAD_HOST = $playwrightHost
}

function Show-Help {
    Write-Host @"
ChampR one-click runner

Usage:
  .\run.ps1 doctor             Check local dependencies
  .\run.ps1 server             Start the backend API (Docker first, then cargo)
  .\run.ps1 crawler [args...]  Run the OP.GG crawler (no args = all champions)
  .\run.ps1 app                Run the desktop client (needs Rust + League client)
  .\run.ps1 all                Start server + crawler

Examples:
  .\run.ps1 server
  .\run.ps1 crawler leesin
  .\run.ps1 crawler --all --mode=aram --output=./output/aram
"@
}

function Show-Doctor {
    Write-Step 'Environment check'

    $checks = @(
        @{ Label = 'node'; Name = 'node' },
        @{ Label = 'npm'; Name = 'npm.cmd' },
        @{ Label = 'corepack'; Name = 'corepack.cmd' },
        @{ Label = 'cargo'; Name = 'cargo' },
        @{ Label = 'docker'; Name = 'docker' },
        @{ Label = 'just'; Name = 'just' }
    )

    foreach ($item in $checks) {
        if (Test-Cmd $item.Name) {
            Write-Host ("  [OK]      {0}" -f $item.Label) -ForegroundColor Green
        }
        else {
            Write-Warn ("  [MISSING] {0}" -f $item.Label)
        }
    }

    Write-Host ''
    Write-Step 'Runnable components'
    Write-Host '  server   -> docker or cargo'
    Write-Host '  crawler  -> node + corepack'
    Write-Host '  app      -> cargo (requires the League client to be running)'
}

function Get-PnpmCommand {
    Set-NodeMirrors

    if (Test-Cmd 'pnpm.cmd') {
        return 'pnpm.cmd'
    }

    if (-not (Test-Cmd 'corepack.cmd')) {
        throw 'pnpm or corepack was not found. Install Node.js 20+ (corepack is bundled with it).'
    }

    $env:COREPACK_HOME = Join-Path $RepoRoot '.cache\corepack'
    return 'corepack.cmd'
}

function Invoke-Pnpm {
    param([string[]]$PnpmArgs)

    if ((Test-Cmd 'pnpm.cmd')) {
        & 'pnpm.cmd' @PnpmArgs
        if ($LASTEXITCODE -ne 0) {
            throw ("pnpm failed with exit code {0}" -f $LASTEXITCODE)
        }
        return
    }

    & 'corepack.cmd' 'pnpm' @PnpmArgs
    if ($LASTEXITCODE -ne 0) {
        throw ("corepack pnpm failed with exit code {0}" -f $LASTEXITCODE)
    }
}

function Start-Server {
    # Reuse a healthy backend, but replace it when the binary on disk is newer than the
    # process that is running - otherwise backend fixes would never take effect until a
    # reboot. Killing it is safe now: the previous launcher checks for "server listening"
    # before treating a non-zero exit as a failure (see below).
    if (Get-NetTCPConnection -LocalPort 3030 -State Listen -ErrorAction SilentlyContinue) {
        $runningServer = Get-Process server -ErrorAction SilentlyContinue | Select-Object -First 1
        $serverBinary = Join-Path $RepoRoot 'target\debug\server.exe'
        $stale = $false
        if ($runningServer -and (Test-Path $serverBinary)) {
            try {
                $stale = (Get-Item $serverBinary).LastWriteTime -gt $runningServer.StartTime
            }
            catch {
                $stale = $false
            }
        }
        if (-not $stale) {
            Write-Step 'Backend already listening on 3030, reuse it'
            return
        }
        Write-Step 'Backend binary is newer than the running server, restarting it'
        Stop-Process -Id $runningServer.Id -Force -ErrorAction SilentlyContinue
        $freeDeadline = (Get-Date).AddSeconds(5)
        while ((Get-Date) -lt $freeDeadline) {
            if (-not (Get-NetTCPConnection -LocalPort 3030 -State Listen -ErrorAction SilentlyContinue)) {
                break
            }
            Start-Sleep -Milliseconds 250
        }
    }

    # docker CLI present != daemon up. PS5.1 traps: no-BOM file parses as ANSI so
    # non-ASCII comments can break the parser; and with ErrorActionPreference=Stop,
    # redirecting stderr of a native command (2>$null) throws NativeCommandError.
    # Probe must stay ASCII-commented, try/catch wrapped, no stderr redirection.
    if (Test-Cmd 'docker') {
        $dockerAlive = $false
        try {
            & docker version --format '{{.Server.Version}}' | Out-Null
            $dockerAlive = ($LASTEXITCODE -eq 0)
        } catch {
            $dockerAlive = $false
        }
        if ($dockerAlive) {
            Write-Step 'Starting the backend with Docker Compose'
            & docker compose up -d --build
            if ($LASTEXITCODE -ne 0) {
                throw ("docker compose failed with exit code {0}" -f $LASTEXITCODE)
            }
            Write-Step 'Backend start requested. Health check: http://127.0.0.1:3030/health'
            return
        }
        Write-Step 'Docker installed but daemon not running, falling back to cargo'
    }

    if (Test-Cmd 'cargo') {
        Write-Step 'Building the backend (offline) and starting it'
        Initialize-MsvcEnvironment
        # --offline: 在线解析依赖会在网络不通时长时间挂住并霸占构建锁(见 Start-App 注释)
        & cargo build --offline -p server
        if ($LASTEXITCODE -ne 0) {
            throw ("cargo build -p server failed with exit code {0}" -f $LASTEXITCODE)
        }
        $serverExe = Join-Path $RepoRoot 'target\debug\server.exe'
        $serverLog = Join-Path $RepoRoot '.cache\server.log'
        Write-Step "Running $serverExe"
        # Hidden console: keep the output in a file so failures can be shown in a dialog.
        # EAP=Continue for the same reason as in Start-App: redirecting stderr of a native
        # command under ErrorActionPreference=Stop throws NativeCommandError in PS 5.1.
        $previousEap = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        & $serverExe *>> $serverLog
        $serverCode = $LASTEXITCODE
        $ErrorActionPreference = $previousEap

        # A non-zero code is NOT a failure when this run did bind the port: it means
        # somebody stopped the server while we were waiting (the launcher used to do that
        # itself and reported a bogus failure, incident 2026-10-05).
        $boundPort = $false
        if (Test-Path $serverLog) {
            $tail = @(Get-Content $serverLog -Tail 120 -ErrorAction SilentlyContinue)
            $boundPort = [bool]($tail | Select-String -Pattern 'server listening' -Quiet)
        }
        if ($boundPort) {
            Write-Step ("server exited with code {0} after binding 3030; treating it as an external stop" -f $serverCode)
            return
        }
        if ($serverCode -ne 0) {
            throw ("server failed with exit code {0}" -f $serverCode)
        }
        return
    }

    throw 'Neither docker (daemon up) nor cargo is available, so the backend cannot be started.'
}

function Start-Crawler {
    Set-NodeMirrors

    Push-Location $RepoRoot
    try {
        if (-not (Test-Path 'node_modules')) {
            Write-Step 'Installing Node dependencies (pnpm install)'
            Invoke-Pnpm -PnpmArgs @('install')
        }

        Push-Location (Join-Path $RepoRoot 'packages\opgg')
        try {
            $crawlerArgs = @()
            if ($RemainingArgs.Count -eq 0) {
                $crawlerArgs = @('--all')
            }
            else {
                $crawlerArgs = $RemainingArgs
            }

            Write-Step 'Running the OP.GG crawler'
            $pnpmArgs = @('start') + $crawlerArgs
            Invoke-Pnpm -PnpmArgs $pnpmArgs
        }
        finally {
            Pop-Location
        }
    }
    finally {
        Pop-Location
    }
}

# Close a running ChampR instead of killing it. taskkill gives exit code 1, which the
# previous launcher process could only read as "the app failed" - that produced scary
# false-alarm dialogs whenever the user double-clicked the icon again (2026-10-05).
# A window close makes the app exit with code 0, so the old launcher stays quiet.
# ASCII-only comments: PS 5.1 parses a BOM-less .ps1 as ANSI.
function Stop-RunningChampR {
    $running = @(Get-Process champr -ErrorAction SilentlyContinue)
    if ($running.Count -eq 0) {
        return
    }

    Write-Step ("Closing {0} running ChampR instance(s) to load the newest build" -f $running.Count)
    foreach ($proc in $running) {
        try {
            $null = $proc.CloseMainWindow()
        }
        catch {
            Write-Warn ("CloseMainWindow failed for pid {0}" -f $proc.Id)
        }
    }

    $deadline = (Get-Date).AddSeconds(6)
    while ((Get-Date) -lt $deadline) {
        if (@(Get-Process champr -ErrorAction SilentlyContinue).Count -eq 0) {
            return
        }
        Start-Sleep -Milliseconds 250
    }

    # Still there (hung or no window): force it, but say so in the log
    Get-Process champr -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Warn ("forcing exit of champr pid {0}" -f $_.Id)
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 500
}

function Start-App {
    if (-not (Test-Cmd 'cargo')) {
        throw 'cargo was not found, so the desktop client cannot be built. Install Rust first.'
    }

    Stop-RunningChampR

    Set-NodeMirrors
    Push-Location $RepoRoot
    try {
        if (-not (Test-Path 'node_modules')) {
            Write-Step 'Installing Node dependencies (pnpm install)'
            Invoke-Pnpm -PnpmArgs @('install')
        }
        # Rebuild the sidecar only when sources are newer than the built output: running
        # tsc on every launch wasted 10-30s and looked like "double-click does nothing".
        # Compare newest source FILE against newest output FILE - the dist directory's
        # own mtime does not change when a file inside it is rewritten, which made the
        # earlier check always true (rebuilt on every single launch, 2026-10-05).
        $webDist = 'packages\deepseek-web\dist'
        $webSrc = 'packages\deepseek-web\src'
        $needsWeb = -not (Test-Path $webDist)
        if (-not $needsWeb -and (Test-Path $webSrc)) {
            $newestSrc = Get-ChildItem $webSrc -Recurse -File -ErrorAction SilentlyContinue |
                Sort-Object LastWriteTime -Descending | Select-Object -First 1
            $newestDist = Get-ChildItem $webDist -Recurse -File -ErrorAction SilentlyContinue |
                Sort-Object LastWriteTime -Descending | Select-Object -First 1
            if ($newestSrc -and (-not $newestDist -or $newestSrc.LastWriteTime -gt $newestDist.LastWriteTime)) {
                $needsWeb = $true
            }
        }
        if ($needsWeb) {
            Write-Step 'Building the DeepSeek Web sidecar'
            Invoke-Pnpm -PnpmArgs @('--dir', 'packages/deepseek-web', 'build')
        }
        else {
            Write-Step 'DeepSeek Web sidecar is up to date, skipping build'
        }
    }
    finally {
        Pop-Location
    }

    Initialize-MsvcEnvironment
    # --offline is mandatory: online, cargo refreshes the registry index, and when the
    # network is down (proxy off) it hangs for a long time while HOLDING the target build
    # lock - after which every launch blocks on "Blocking waiting for file lock on build
    # directory", which looks exactly like "double-click and nothing happens"
    # (real incident 2026-10-04). Offline builds use the local cache and take seconds;
    # a genuinely new dependency now fails loudly instead of hanging silently.
    # Keep comments ASCII here: PS 5.1 reads a BOM-less .ps1 as ANSI and CJK can break it.
    Write-Step 'Building the desktop client (offline)'
    & cargo build --offline -p champr --bin champr
    if ($LASTEXITCODE -ne 0) {
        throw ("cargo build -p champr failed with exit code {0}" -f $LASTEXITCODE)
    }
    # App runs hidden now, so "exit fast" must raise a dialog instead of a console tail.
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    # The app logs to .cache/champr.log itself; the console copy is redirected for the
    # record. EAP must be Continue around it: with Stop, redirecting a native command's
    # stderr makes PS 5.1 throw NativeCommandError and the launcher reports a bogus
    # failure whose message is just the app's first log line (hit twice, 2026-10-05).
    $appExe = Join-Path $RepoRoot 'target\debug\champr.exe'
    $appLog = Join-Path $RepoRoot '.cache\champr.log'
    Write-Step "Starting $appExe"
    $previousEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $appExe *>> (Join-Path $RepoRoot '.cache\app-console.log')
    $appCode = $LASTEXITCODE
    $ErrorActionPreference = $previousEap
    $stopwatch.Stop()

    # Fast exit only counts as a failure when the app never got as far as showing its
    # window (checked in the current run's own log section). A quick exit AFTER a healthy
    # "main window placed" means somebody closed or restarted it, which is not an error
    # and must not raise a modal dialog (false alarms on 2026-10-05).
    if ($stopwatch.Elapsed.TotalSeconds -lt 5) {
        $healthyStart = $false
        if (Test-Path $appLog) {
            $tail = @(Get-Content $appLog -Tail 80 -ErrorAction SilentlyContinue)
            $startIndex = -1
            for ($i = $tail.Count - 1; $i -ge 0; $i--) {
                if ($tail[$i] -match 'ChampR starting') { $startIndex = $i; break }
            }
            if ($startIndex -ge 0) {
                $section = $tail[$startIndex..($tail.Count - 1)]
                $healthyStart = [bool]($section | Select-String -Pattern 'main window placed' -Quiet)
            }
        }

        if ($healthyStart) {
            Write-Step ("champr was closed {0:N1}s after a healthy start; not reporting a failure" -f $stopwatch.Elapsed.TotalSeconds)
        }
        else {
            Show-Failure -Title 'ChampR exited immediately' -Message ("champr exited after {0:N1}s with code {1} before showing its window" -f $stopwatch.Elapsed.TotalSeconds, $appCode) -LogPath $appLog
        }
    }

    if ($appCode -ne 0 -and $appCode -ne 1) {
        throw ("champr exited with code {0} (log: {1})" -f $appCode, $appLog)
    }
}

try {
    switch ($Command) {
        'doctor'  { Show-Doctor }
        'server'  { Start-Server }
        'crawler' { Start-Crawler }
        'app'     { Start-App }
        'all' {
            Start-Server
            Start-Crawler
        }
        'help' { Show-Help }
    }
}
catch {
    $reason = $_.Exception.Message
    Write-Warn ("Execution failed: {0}" -f $reason)
    $logForCommand = if ($Command -eq 'server') { '.cache\server.log' } else { '.cache\champr.log' }
    Show-Failure -Title ("ChampR {0} failed" -f $Command) -Message $reason -LogPath (Join-Path $RepoRoot $logForCommand)
    exit 1
}
