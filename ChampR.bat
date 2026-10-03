@echo off
cd /d "%~dp0"
rem Already running? The running exe locks target\debug\champr.exe, so cargo cannot
rem relink it and the user just sees the old window. Offer to close it and rebuild
rem instead of dead-ending (that dead end wasted a whole round of debugging).
tasklist /fi "imagename eq champr.exe" 2>nul | find /i "champr.exe" >nul
if not errorlevel 1 (
  echo.
  echo ChampR is running - its exe is locked, so the newest build cannot be written.
  choice /c YN /n /m "Close the running ChampR and rebuild now? [Y/N] "
  if errorlevel 2 (
    echo.
    echo    Kept it running. To reload the newest build later: tray right-click,
    echo    Exit ChampR, then start this again.
    timeout /t 6 >nul
    exit /b 0
  )
  echo Closing the running instance...
  taskkill /im champr.exe /f >nul 2>&1
  timeout /t 2 >nul
)
start "ChampR Server" powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 server
rem Wait for the backend port (max 90s). No server = runes and counter stay silent.
powershell -NoProfile -Command "$deadline = (Get-Date).AddSeconds(90); while ((Get-Date) -lt $deadline) { if (Get-NetTCPConnection -LocalPort 3030 -State Listen -ErrorAction SilentlyContinue) { exit 0 }; Start-Sleep -Milliseconds 500 }"
powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 app
