@echo off
cd /d "%~dp0"
start "ChampR Server" powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 server
timeout /t 2 /nobreak >nul
powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 app
