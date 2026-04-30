# VCC install script for Windows
# Usage: irm https://raw.githubusercontent.com/ejfkdev/vcc-cli/main/install.ps1 | iex

$ErrorActionPreference = "Stop"

$Repo = "ejfkdev/vcc-cli"
$InstallDir = if ($env:VCC_INSTALL_DIR) { $env:VCC_INSTALL_DIR } else { "$env:LOCALAPPDATA\vcc" }
$BinPath = Join-Path $InstallDir "vcc.exe"

# Detect architecture
$Arch = if ([Environment]::Is64BitOperatingSystem) { "x86_64" } else { "x86" }
$Artifact = "vcc-${Arch}-windows.exe"

# Get latest release tag
$Release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
$Tag = $Release.tag_name -replace '^v',''

Write-Host "Installing vcc v$Tag for Windows..."

# Download
$Url = "https://github.com/$Repo/releases/download/v$Tag/$Artifact"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Invoke-WebRequest -Uri $Url -OutFile $BinPath -UseBasicParsing

# Add to PATH if not already present
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
    $env:Path = "$env:Path;$InstallDir"
}

Write-Host "vcc v$Tag installed to $BinPath"
& $BinPath --version
