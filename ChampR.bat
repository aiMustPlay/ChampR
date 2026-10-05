@echo off
cd /d "%~dp0"
rem ChampR launcher. It leaves NO visible console behind:
rem   * server and app run hidden, logs go to .cache\server.log and .cache\champr.log
rem   * if a step fails, run.ps1 pops a message box with the reason (Show-Failure)
rem   * the app has a LOG button in its footer that opens .cache\champr.log
rem Keep every comment and string here ASCII: cmd.exe parses .bat in GBK, and a CJK
rem comment line gets shredded into garbage commands (real incident 2026-10-03).

rem A cargo left over from an earlier launch can sit forever on a network resolve while
rem holding the target build lock, after which every launch hangs silently on
rem "Blocking waiting for file lock on build directory" (real incident 2026-10-04).
tasklist /fi "imagename eq cargo.exe" 2>nul | find /i "cargo.exe" >nul
if not errorlevel 1 (
  taskkill /im cargo.exe /f >nul 2>&1
  timeout /t 1 >nul
)

rem The running app locks target\debug\champr.exe, so cargo cannot relink it.
tasklist /fi "imagename eq champr.exe" 2>nul | find /i "champr.exe" >nul
if not errorlevel 1 (
  taskkill /im champr.exe /f >nul 2>&1
  timeout /t 2 >nul
)

rem Restart the backend as well, so server-side changes take effect on every launch.
tasklist /fi "imagename eq server.exe" 2>nul | find /i "server.exe" >nul
if not errorlevel 1 (
  taskkill /im server.exe /f >nul 2>&1
  timeout /t 1 >nul
)

if not exist ".cache" mkdir ".cache"
rem Hidden launcher processes; their output goes to the log files below.
start "" /min powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File .\run.ps1 server
start "" /min powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File .\run.ps1 app
exit /b 0
