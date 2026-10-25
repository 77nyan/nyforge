@echo off
setlocal
rem NyForge installer for Windows. Double-click it, or run it in a terminal. No administrator needed.
rem   install.cmd              install for the current user (Start menu entry, command line, "Apps" entry)
rem   install.cmd -uninstall   remove it again (the installed copy is also available as uninstall.cmd)
rem (NYFORGE_NO_PAUSE=1 skips the final "press any key", for automated runs.)
set "DIR=%LOCALAPPDATA%\Programs\NyForge"
if /i "%~1"=="-uninstall" goto uninstall
if /i "%~1"=="/uninstall" goto uninstall

set "SRC=%~dp0"
if not exist "%SRC%nyforge.exe" set "SRC=%~dp0target\release\"
if not exist "%SRC%nyforge.exe" (
  echo Cannot find nyforge.exe next to install.cmd.
  echo Download the Windows zip from the Releases page, extract all files, and run install.cmd from that folder.
  goto fail
)
rem Files from the internet are marked as untrusted; clear that for this folder.
powershell -NoProfile -Command "Get-ChildItem -LiteralPath '%~dp0' -Recurse -File | Unblock-File" >nul 2>&1

taskkill /f /im nyforge-gui.exe >nul 2>&1
if not exist "%DIR%" mkdir "%DIR%"
copy /y "%SRC%nyforge.exe" "%DIR%\" >nul || goto fail
if exist "%SRC%nyforge-gui.exe" copy /y "%SRC%nyforge-gui.exe" "%DIR%\" >nul || goto fail
for %%F in (LICENSE README.md THIRD-PARTY.md) do if exist "%~dp0%%F" copy /y "%~dp0%%F" "%DIR%\" >nul

rem The uninstaller: a copy of this file plus a tiny uninstall.cmd, kept in the install folder.
if /i not "%~dp0"=="%DIR%\" copy /y "%~f0" "%DIR%\install.cmd" >nul
> "%DIR%\uninstall.cmd" echo @echo off
>> "%DIR%\uninstall.cmd" echo call "%%~dp0install.cmd" -uninstall
>> "%DIR%\uninstall.cmd" echo exit /b

powershell -NoProfile -Command "$d=$env:DIR; $l=Join-Path ([Environment]::GetFolderPath('Programs')) 'NyForge.lnk'; $s=(New-Object -ComObject WScript.Shell).CreateShortcut($l); $s.TargetPath=Join-Path $d 'nyforge-gui.exe'; $s.WorkingDirectory=$d; $s.Description='NyForge'; $s.Save(); $p=[Environment]::GetEnvironmentVariable('Path','User'); if(-not (($p -split ';') | Where-Object { $_.TrimEnd('\') -eq $d })){ [Environment]::SetEnvironmentVariable('Path',($p.TrimEnd(';')+';'+$d),'User') }; $k='HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\NyForge'; New-Item -Force -Path $k | Out-Null; Set-ItemProperty $k DisplayName 'NyForge'; Set-ItemProperty $k Publisher 'NyForge'; Set-ItemProperty $k InstallLocation $d; Set-ItemProperty $k UninstallString ([char]34 + (Join-Path $d 'uninstall.cmd') + [char]34); Set-ItemProperty $k NoModify 1 -Type DWord; Set-ItemProperty $k NoRepair 1 -Type DWord" || goto fail

echo.
echo NyForge installed in %DIR%
echo Open "NyForge" from the Start menu. In a new terminal you can also run: nyforge --version
echo To remove it: Settings ^> Apps ^> NyForge ^> Uninstall, or run "%DIR%\uninstall.cmd".
echo.
echo If Windows Defender or another antivirus removes or blocks NyForge, add this folder to its exclusions:
echo   %DIR%
goto end

:uninstall
taskkill /f /im nyforge-gui.exe >nul 2>&1
powershell -NoProfile -Command "$d=$env:DIR; Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path ([Environment]::GetFolderPath('Programs')) 'NyForge.lnk'); Remove-Item -Recurse -Force -ErrorAction SilentlyContinue 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\NyForge'; $p=[Environment]::GetEnvironmentVariable('Path','User'); $n=(($p -split ';') | Where-Object { $_ -and $_.TrimEnd('\') -ne $d }) -join ';'; [Environment]::SetEnvironmentVariable('Path',$n,'User'); Get-ChildItem -LiteralPath $d -Force -ErrorAction SilentlyContinue | Where-Object { $_.Name -notin 'install.cmd','uninstall.cmd' } | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue"
rem This script lives in the folder it removes, so the folder goes a moment after it has finished.
if exist "%DIR%" start "" /b cmd /c ping -n 3 127.0.0.1 ^>nul ^& rmdir /s /q "%DIR%"
echo NyForge removed from %DIR%.
goto end

:fail
echo.
echo The installation did not finish.
endlocal
if not defined NYFORGE_NO_PAUSE pause
exit /b 1

:end
endlocal
if not defined NYFORGE_NO_PAUSE pause
