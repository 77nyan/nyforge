# NyForge installer for Windows.
#
#   install.cmd (double-click)    install for the current user, no administrator needed (starts this script safely)
#   .\install.ps1                  the same, if PowerShell allows unsigned scripts
#   .\install.ps1 -Uninstall       remove it again
#   .\install.ps1 -Dir C:\Tools\NyForge
#
# Installs the NyForge app (nyforge-gui.exe, with a Start menu shortcut) and the command-line tool (nyforge.exe).
# Writing to drives needs administrator rights: the app offers "Restart as administrator", and the
# command line needs a terminal opened with "Run as administrator".
param(
  [switch]$Uninstall,
  [string]$Dir = (Join-Path $env:LOCALAPPDATA 'Programs\NyForge')
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

function Get-UserPath { [Environment]::GetEnvironmentVariable('Path', 'User') }
function Set-UserPath([string]$value) { [Environment]::SetEnvironmentVariable('Path', $value, 'User') }
function Split-PathList([string]$p) { if ($p) { $p.Split(';') | Where-Object { $_ -ne '' } } else { @() } }

if ($Uninstall) {
  if (Test-Path $Dir) { Remove-Item -Recurse -Force $Dir }
  $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'NyForge.lnk'
  if (Test-Path $lnk) { Remove-Item -Force $lnk }
  $rest = Split-PathList (Get-UserPath) | Where-Object { $_.TrimEnd('\') -ne $Dir.TrimEnd('\') }
  Set-UserPath ($rest -join ';')
  Write-Host "NyForge removed from $Dir."
  exit 0
}

$exe = Join-Path $here 'nyforge.exe'
if (-not (Test-Path $exe)) {
  $exe = Join-Path $here 'target\release\nyforge.exe'
}
if (-not (Test-Path $exe)) {
  throw "Cannot find nyforge.exe next to install.ps1. Download a release zip, or build with: cargo build --release -p nyforge -p nyforge-gui"
}

$gui = Join-Path (Split-Path -Parent $exe) 'nyforge-gui.exe'
if (-not (Test-Path $gui)) { $gui = $null }

New-Item -ItemType Directory -Force -Path $Dir | Out-Null
Copy-Item $exe (Join-Path $Dir 'nyforge.exe') -Force
if ($gui) {
  Copy-Item $gui (Join-Path $Dir 'nyforge-gui.exe') -Force
  $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'NyForge.lnk'
  $shell = New-Object -ComObject WScript.Shell
  $sc = $shell.CreateShortcut($lnk)
  $sc.TargetPath = Join-Path $Dir 'nyforge-gui.exe'
  $sc.WorkingDirectory = $Dir
  $sc.Description = 'NyForge: flash images, multiboot drives, format USB drives'
  $sc.Save()
  Write-Host "Start menu shortcut created: NyForge"
}
foreach ($f in 'LICENSE', 'README.md', 'THIRD-PARTY.md') {
  $src = Join-Path $here $f
  if (Test-Path $src) { Copy-Item $src $Dir -Force }
}

$parts = @(Split-PathList (Get-UserPath))
if (-not ($parts | Where-Object { $_.TrimEnd('\') -eq $Dir.TrimEnd('\') })) {
  Set-UserPath ((@($parts) + $Dir) -join ';')
  Write-Host "Added $Dir to your PATH (open a new terminal to use it)."
}
Write-Host "NyForge installed in $Dir"
if ($gui) { Write-Host "Start NyForge from the Start menu." }
Write-Host "Command line:  nyforge list"
