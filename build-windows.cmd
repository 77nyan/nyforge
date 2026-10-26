@echo off
setlocal
rem Builds NyForge from this source folder and installs it for the current user. Double-click it.
rem Needs Rust (https://rustup.rs) and, if asked, the "Desktop development with C++" part of Visual Studio Build Tools.
cd /d "%~dp0"
where cargo >nul 2>&1 || (
  echo Rust is not installed. Get it from https://rustup.rs , then run this file again.
  pause
  exit /b 1
)
echo Building NyForge. The first build takes a few minutes...
cargo build --release || (
  echo.
  echo The build failed. Copy the messages above and send them to me.
  pause
  exit /b 1
)
call "%~dp0install.cmd"
