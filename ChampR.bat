@echo off
cd /d "%~dp0"
rem ChampR launcher. It leaves NO visible console behind:
rem   * server and app run hidden, logs go to .cache\server.log and .cache\champr.log
rem   * if a step fails, run.ps1 pops a message box with the reason (Show-Failure)
rem   * the app has a LOG button in its footer that opens .cache\champr.log
rem Keep every comment and string here ASCII: cmd.exe parses .bat in GBK, and a CJK
rem comment line gets shredded into garbage commands (real incident 2026-10-03).

rem DO NOT kill cargo.exe here. Doing that killed the build of a launcher that was
rem already working: the user double-clicks again, this batch taskkills every cargo,
rem and the first launcher sees its own build die with exit code 1 and reports a bogus
rem "build failed" (incident 2026-10-05). Stale cargos are handled in run.ps1, which
rem only reaps ones that have been alive for minutes, never an active build.

rem The running app is closed by run.ps1 (Stop-RunningChampR) with a graceful window
rem close, NOT taskkill: taskkill reports exit code 1, which the previous launcher can
rem only read as a failure and turns into a false-alarm dialog on every re-launch.

rem The backend is NOT killed here: run.ps1 reuses it when port 3030 is already
rem listening, and killing a server process makes the previous launcher read exit
rem code 1 as a failure and pop a bogus dialog (incident 2026-10-05). To force a
rem backend restart, end server.exe manually and start this launcher again.

if not exist ".cache" mkdir ".cache"
rem Hidden launcher processes; their output goes to the log files below.
start "" /min powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File .\run.ps1 server
start "" /min powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File .\run.ps1 app
exit /b 0
