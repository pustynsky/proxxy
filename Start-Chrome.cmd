@echo off
powershell.exe -NoProfile -ExecutionPolicy RemoteSigned -File "%~dp0Start-Chrome.ps1"
if errorlevel 1 pause
