@echo off
cd /d "%~dp0"
rem Already running? Then the exe is locked and cargo cannot relink it - the user
rem would just see the old window and think the update did nothing. Say so instead.
tasklist /fi "imagename eq champr.exe" 2>nul | find /i "champr.exe" >nul
if not errorlevel 1 (
  echo.
  echo ChampR is already running - the running exe locks target\debug\champr.exe,
  echo so a new build cannot be written.
  echo.
  echo   * To keep using it: click the tray icon to bring the window back.
  echo   * To load the newest build: tray right-click - Exit ChampR, then run this again.
  echo.
  pause
  exit /b 0
)
start "ChampR Server" powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 server
rem Wait for the backend port (max 90s). No server = runes and counter stay silent.
powershell -NoProfile -Command "$deadline = (Get-Date).AddSeconds(90); while ((Get-Date) -lt $deadline) { if (Get-NetTCPConnection -LocalPort 3030 -State Listen -ErrorAction SilentlyContinue) { exit 0 }; Start-Sleep -Milliseconds 500 }"
powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 app
