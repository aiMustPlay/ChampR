@echo off
cd /d "%~dp0"
start "ChampR Server" powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 server
rem 等 server 就绪(最多 90 秒; 没有 server 符文/counter 数据全是哑的)
powershell -NoProfile -Command "$deadline = (Get-Date).AddSeconds(90); while ((Get-Date) -lt $deadline) { if (Get-NetTCPConnection -LocalPort 3030 -State Listen -ErrorAction SilentlyContinue) { exit 0 }; Start-Sleep -Milliseconds 500 }"
powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 app
