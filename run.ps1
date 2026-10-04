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
}

function Write-Warn {
    param([string]$Message)
    Write-Host "!! $Message" -ForegroundColor Yellow
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
    # 已在跑就静默复用, 不要二连启动
    if (Get-NetTCPConnection -LocalPort 3030 -State Listen -ErrorAction SilentlyContinue) {
        Write-Step 'Backend already listening on 3030, reuse it'
        return
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
        Write-Step "Running $serverExe"
        & $serverExe
        if ($LASTEXITCODE -ne 0) {
            throw ("server failed with exit code {0}" -f $LASTEXITCODE)
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

function Start-App {
    if (-not (Test-Cmd 'cargo')) {
        throw 'cargo was not found, so the desktop client cannot be built. Install Rust first.'
    }

    Set-NodeMirrors
    Push-Location $RepoRoot
    try {
        if (-not (Test-Path 'node_modules')) {
            Write-Step 'Installing Node dependencies (pnpm install)'
            Invoke-Pnpm -PnpmArgs @('install')
        }
        Write-Step 'Building the DeepSeek Web sidecar'
        Invoke-Pnpm -PnpmArgs @('--dir', 'packages/deepseek-web', 'build')
    }
    finally {
        Pop-Location
    }

    Initialize-MsvcEnvironment
    # --offline 是硬性要求: 在线时 cargo 会去更新 registry index, 网络不通(代理没开)会
    # 长时间挂住, 同时**霸占 target 构建锁** —— 之后任何启动都卡在
    # "Blocking waiting for file lock on build directory", 表现就是双击后什么都没有
    # (2026-10-04 实际事故)。离线构建依赖已全部在本地缓存, 秒级完成;
    # 若真新增了依赖, 这里会明确报错而不是静默挂死。
    Write-Step 'Building the desktop client (offline)'
    & cargo build --offline -p champr --bin champr
    if ($LASTEXITCODE -ne 0) {
        throw ("cargo build -p champr failed with exit code {0}" -f $LASTEXITCODE)
    }
    # 直接跑二进制而不是 cargo run: cargo run 会作为父进程常驻, 也多一层"哪个 bin"的坑
    $appExe = Join-Path $RepoRoot 'target\debug\champr.exe'
    Write-Step "Starting $appExe"
    & $appExe
    if ($LASTEXITCODE -ne 0) {
        throw ("champr exited with code {0}" -f $LASTEXITCODE)
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
    Write-Warn ("Execution failed: {0}" -f $_.Exception.Message)
    exit 1
}
