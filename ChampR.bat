@echo off
cd /d "%~dp0"
start "ChampR Server" powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 server
rem Wait for backend port (max 90s). No server = runes and counter stay silent.
powershell -NoProfile -Command "$deadline = (Get-Date).AddSeconds(90); while ((Get-Date) -lt $deadline) { if (Get-NetTCPConnection -LocalPort 3030 -State Listen -ErrorAction SilentlyContinue) { exit 0 }; Start-Sleep -Milliseconds 500 }"
powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 app
