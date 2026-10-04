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
rem Launch the app in its own window and exit immediately. The old flow waited up
rem to 90s for port 3030 inside THIS console; closing the console during that wait
rem killed the launcher before the app step ran, which looked exactly like "the app
rem does nothing" (2026-10-04 forensics: server up, no cargo run -p champr). Nothing
rem here is load-bearing now: the app retries its champion-list fetch until the
rem backend answers, so a slow server is fine.
start "ChampR" powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 app
exit /b 0
