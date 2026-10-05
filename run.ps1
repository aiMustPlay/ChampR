param(
    [Parameter(Position = 0)]
    [ValidateSet('doctor', 'server', 'crawler', 'app', 'all', 'help')]
    [string]$Command = 'doctor',

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$RemainingArgs
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
# Captured here on purpose: inside a function $MyInvocation describes the FUNCTION call,
# so $MyInvocation.MyCommand.Path is null there and self-elevation silently failed
# (2026-10-05: "elevation refused ... ArgumentList is null").
$ScriptPath = $MyInvocation.MyCommand.Path

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

# Hidden launcher = nobody sees the console, so a real failure must surface on its own.
# Keep the dialog SHORT: one sentence plus an optional tiny log tail for genuine build
# errors. A 20-line log dump that has nothing to do with the problem is just noise and
# got the user (rightly) angry on 2026-10-05.
# ASCII-only comments here: PS 5.1 reads a BOM-less .ps1 as ANSI, CJK can break parsing.
function Show-Failure {
    param(
        [string]$Title,
        [string]$Message,
        [string]$LogPath,
        # 0 = no log attachment; only genuine build errors pass a small number
        [int]$TailLines = 0
    )

    Add-Content -Path (Join-Path $RepoRoot '.cache\launcher.log') -Value ("FAIL " + $Title + ": " + $Message) -ErrorAction SilentlyContinue

    $full = $Message
    if ($TailLines -gt 0 -and $LogPath -and (Test-Path $LogPath)) {
        $tail = Get-Content $LogPath -Tail $TailLines -ErrorAction SilentlyContinue
        if ($tail) {
            $full = $Message + "`n`n" + ($tail -join "`n")
        }
    }

    try {
        Add-Type -AssemblyName System.Windows.Forms -ErrorAction Stop
        [System.Windows.Forms.MessageBox]::Show($full, $Title, 'OK', 'Error') | Out-Null
    }
    catch {
        Write-Warn $Message
    }
}

# One short question only. Returns $true when the user agrees.
function Request-YesNo {
    param([string]$Title, [string]$Message)

    try {
        Add-Type -AssemblyName System.Windows.Forms -ErrorAction Stop
        $result = [System.Windows.Forms.MessageBox]::Show($Message, $Title, 'OKCancel', 'Question')
        return ($result -eq [System.Windows.Forms.DialogResult]::OK)
    }
    catch {
        Write-Warn $Message
        return $false
    }
}

# Is the built binary older than the sources (i.e. a rebuild would change it)?
# Do not compare exe vs process start: the process runs that very exe, so it never looks stale.
function Test-AppBinaryStale {
    $exe = Join-Path $RepoRoot 'target\debug\champr.exe'
    if (-not (Test-Path $exe)) {
        return $true
    }
    $exeTime = (Get-Item $exe).LastWriteTime
    $patterns = @(
        'crates\app\src\*.rs',
        'crates\app\src\**\*.rs',
        'crates\app\ui\*.slint',
        'crates\app\build.rs',
        'crates\app\Cargo.toml',
        'crates\lcu\src\*.rs',
        'crates\lcu\src\**\*.rs',
        'crates\lcu\Cargo.toml',
        'Cargo.toml',
        'Cargo.lock'
    )
    foreach ($pattern in $patterns) {
        $files = Get-ChildItem $pattern -File -ErrorAction SilentlyContinue
        foreach ($file in $files) {
            if ($file.LastWriteTime -gt $exeTime) {
                return $true
            }
        }
    }
    return $false
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
        $script:ServerLauncherMutex = Enter-LauncherMutex 'server'
        Write-Step 'Building the backend (offline) and starting it'
        Initialize-MsvcEnvironment
        # --offline: online dependency resolution hangs while holding the build lock (see Start-App)
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

# Only one launcher may do a given job at a time.
# Double-clicking the icon twice starts two `run.ps1 app` processes; the second one
# relinks target\debug\champr.exe while the first has already started it, and cargo
# fails with "failed to remove file" (exit 101) - a confusing dialog that says nothing
# about the real cause (2026-10-05). With the mutex the late one just exits quietly.
function Enter-LauncherMutex {
    param([string]$Name)

    $mutex = New-Object System.Threading.Mutex($false, "Local\ChampR-Launcher-$Name")
    if (-not $mutex.WaitOne(0)) {
        Write-Step ("another ChampR {0} launcher is already running; nothing to do" -f $Name)
        exit 0
    }
    return $mutex
}

# Close a running ChampR instead of killing it. taskkill gives exit code 1, which the
# previous launcher process could only read as "the app failed" - that produced scary
# false-alarm dialogs whenever the user double-clicked the icon again (2026-10-05).
# A window close makes the app exit with code 0, so the old launcher stays quiet.
# ASCII-only comments: PS 5.1 parses a BOM-less .ps1 as ANSI.
# Try to close a running ChampR so the newest build can be linked.
# Returns $true when nothing is running any more (or nothing was running), $false when an
# instance is still alive - typically one started elevated while this launcher is not, and
# a normal-privilege process simply cannot touch it (that is why the shortcut's missing
# RunAs flag kept producing a useless dialog on 2026-10-05).
function Stop-RunningChampR {
    $running = @(Get-Process champr -ErrorAction SilentlyContinue)
    if ($running.Count -eq 0) {
        return $true
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
    if (Wait-ProcessesGone 5) {
        return $true
    }

    # Graceful close did not work: the running build may predate the close-requested
    # handler, or it ignores the message. Force it if we are allowed to.
    Get-Process champr -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Warn ("forcing exit of champr pid {0}" -f $_.Id)
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
    return (Wait-ProcessesGone 3)
}

# Wait until no champr process is left (at most N seconds). $true = all gone.
function Wait-ProcessesGone {
    param([int]$Seconds)

    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (@(Get-Process champr -ErrorAction SilentlyContinue).Count -eq 0) {
            return $true
        }
        Start-Sleep -Milliseconds 250
    }
    return (@(Get-Process champr -ErrorAction SilentlyContinue).Count -eq 0)
}

# Wait for the exe to be unlocked (relinking while it is mapped fails with exit 101).
function Wait-AppBinaryUnlocked {
    param([int]$Seconds)

    $exePath = Join-Path $RepoRoot 'target\debug\champr.exe'
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (@(Get-Process champr -ErrorAction SilentlyContinue).Count -eq 0) {
            try {
                $stream = [System.IO.File]::Open($exePath, 'Open', 'ReadWrite', 'None')
                $stream.Close()
                return $true
            }
            catch {
                # still mapped; keep waiting
            }
        }
        Start-Sleep -Milliseconds 250
    }
    return $false
}

# Re-run the same command elevated (one UAC prompt). Needed when the old instance
# was started elevated and normal privileges cannot close it.
function Restart-LauncherElevated {
    param([string]$Command)

    Write-Step ("relaunching this launcher elevated so it can close the running ChampR ({0})" -f $Command)
    try {
        Start-Process -FilePath 'powershell.exe' -Verb RunAs -ArgumentList @(
            '-NoProfile', '-ExecutionPolicy', 'Bypass',
            '-File', $ScriptPath, $Command
        ) | Out-Null
        return $true
    }
    catch {
        Write-Warn ("elevation refused: {0}" -f $_.Exception.Message)
        return $false
    }
}


function Start-App {
    if (-not (Test-Cmd 'cargo')) {
        throw 'cargo was not found, so the desktop client cannot be built. Install Rust first.'
    }

    # Held for the whole lifetime of this launcher process (do not let it be collected).
    $script:AppLauncherMutex = Enter-LauncherMutex 'app'

    # Already running? Only interfere when the binary on disk is older than the sources,
    # i.e. when the user really would get an outdated UI. Otherwise say nothing at all -
    # a dialog for "it is already running" is noise (2026-10-05: user got a wall of
    # unrelated log lines and was, rightly, annoyed).
    $runningCount = @(Get-Process champr -ErrorAction SilentlyContinue).Count
    if ($runningCount -gt 0) {
        if (-not (Test-AppBinaryStale)) {
            Write-Step 'ChampR is already running the current build; nothing to do'
            exit 0
        }

        if (Stop-RunningChampR) {
            if (-not (Wait-AppBinaryUnlocked 10)) {
                Write-Warn 'binary still locked after closing ChampR; continuing anyway'
            }
        }
        else {
            # Still alive: almost always an elevated instance we cannot touch.
            $answer = Request-YesNo -Title 'ChampR is running' -Message (
                "ChampR is running an older build. It must be closed to load the new one.`n`n" +
                "OK  = retry elevated (one UAC prompt)`nCancel = keep it as it is"
            )
            if ($answer) {
                if (Restart-LauncherElevated -Command 'app') {
                    exit 0
                }
            }
            Write-Step 'user kept the running instance; nothing to do'
            exit 0
        }
    }

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
    $previousBuildEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $buildOutput = & cargo build --offline -p champr --bin champr 2>&1
    $buildCode = $LASTEXITCODE
    $ErrorActionPreference = $previousBuildEap
    if ($buildCode -ne 0) {
        $buildText = ($buildOutput | Out-String)
        # Exit 101 with "failed to remove file" is not a code problem: the old exe is still
        # mapped by a running process. Say that instead of showing raw cargo output.
        if ($buildText -match 'failed to remove file|being used by another process|os error 32') {
            throw 'ChampR is still running (started elevated?) so its exe cannot be replaced. Exit ChampR from the tray icon, then start this launcher again.'
        }
        $buildOutput | Select-Object -Last 15 | ForEach-Object { Write-Warn $_ }
        throw ("cargo build -p champr failed with exit code {0}" -f $buildCode)
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
            Show-Failure -Title 'ChampR failed to start' -Message ("champr exited after {0:N1}s with code {1} before showing its window" -f $stopwatch.Elapsed.TotalSeconds, $appCode) -LogPath $appLog -TailLines 8
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
    Show-Failure -Title ("ChampR {0} failed" -f $Command) -Message $reason -LogPath (Join-Path $RepoRoot $logForCommand) -TailLines 8
    exit 1
}
