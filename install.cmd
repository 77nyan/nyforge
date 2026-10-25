@echo off
rem NyForge installer for Windows: double-click this file (or run it in a terminal).
rem It starts install.ps1 with the script check switched off for this one run, because Windows blocks
rem unsigned PowerShell scripts that came from the internet ("is not digitally signed").
rem Pass -Uninstall to remove NyForge.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Get-ChildItem -LiteralPath %~dp0 -Recurse -File | Unblock-File" >/dev/null 2>&1
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
echo.
pause
