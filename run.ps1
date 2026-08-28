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
    if (Test-Cmd 'docker') {
        Write-Step 'Starting the backend with Docker Compose'
        & docker compose up -d --build
        if ($LASTEXITCODE -ne 0) {
            throw ("docker compose failed with exit code {0}" -f $LASTEXITCODE)
        }
        Write-Step 'Backend start requested. Health check: http://127.0.0.1:3030/health'
        return
    }

    if (Test-Cmd 'cargo') {
        Write-Step 'Starting the backend with cargo'
        Initialize-MsvcEnvironment
        & cargo run -p server
        if ($LASTEXITCODE -ne 0) {
            throw ("cargo run -p server failed with exit code {0}" -f $LASTEXITCODE)
        }
        return
    }

    throw 'Neither docker nor cargo is available, so the backend cannot be started.'
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

    Initialize-MsvcEnvironment
    Write-Step 'Starting the desktop client'
    & cargo run -p champr
    if ($LASTEXITCODE -ne 0) {
        throw ("cargo run -p champr failed with exit code {0}" -f $LASTEXITCODE)
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
