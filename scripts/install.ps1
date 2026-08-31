#Requires -Version 5.1
# snoop installer for Windows.
# Usage: powershell -ExecutionPolicy Bypass -File install.ps1 [-Version latest|tag] [-InstallDir <path>]
# Piped: & ([scriptblock]::Create((irm <raw-url>/scripts/install.ps1))) -Version v0.1.0
param(
    [string]$Version = "latest",
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\snoop"
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# TLS 1.2 needed by Windows PowerShell 5.x on older stacks.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Repo = if ($env:GITHUB_REPO) { $env:GITHUB_REPO } else { 'colbymchenry/snoop' }

# Prefer the real OS architecture: x64-emulated PowerShell on Windows ARM64
# reports AMD64 via $env:PROCESSOR_ARCHITECTURE.
$arch = "$env:PROCESSOR_ARCHITECTURE"
try {
    $osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    if ($osArch) { $arch = "$osArch" }
} catch { }

switch -Regex ($arch) {
    '^(X64|AMD64)$'    { $target = 'x86_64-pc-windows-msvc' }
    '^(ARM64|AARCH64)$' { $target = 'aarch64-pc-windows-msvc' }
    default { throw "Unsupported architecture: $arch" }
}

if ($Version -eq 'latest') {
    $url = "https://github.com/$Repo/releases/latest/download/snoop-$target.zip"
} else {
    $url = "https://github.com/$Repo/releases/download/$Version/snoop-$target.zip"
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("snoop-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
$zip = Join-Path $tmp "snoop-$target.zip"
$extract = Join-Path $tmp 'extracted'

try {
    Write-Host "Downloading snoop ($target) from $url"
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } catch {
        throw "Download failed: $url`n$($_.Exception.Message)"
    }
    if (-not (Test-Path $zip) -or (Get-Item $zip).Length -eq 0) {
        throw "Asset not found or empty: $url"
    }

    Expand-Archive -Path $zip -DestinationPath $extract
    $exe = Get-ChildItem -Path $extract -Recurse -Filter 'snoop.exe' | Select-Object -First 1
    if (-not $exe) { throw "snoop.exe not found in archive" }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Move-Item -Path $exe.FullName -Destination (Join-Path $InstallDir 'snoop.exe') -Force

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @()
    if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ } }
    if ($entries -notcontains $InstallDir) {
        $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "Added $InstallDir to your user PATH. Open a new terminal for it to take effect."
    }
    if (($env:Path -split ';') -notcontains $InstallDir) {
        $env:Path = "$env:Path;$InstallDir"
    }

    $snoop = Join-Path $InstallDir 'snoop.exe'
    try {
        $v = (& $snoop --version | Select-Object -First 1)
        Write-Host "Installed: $v"
    } catch {
        Write-Host "Installed to $snoop (could not run --version locally)."
    }

    Write-Host ""
    Write-Host "Next steps:"
    Write-Host "  1. Open a new terminal."
    Write-Host "  2. Run: snoop install   # wires up your coding agents"
    Write-Host "  3. In each project, run: snoop init"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
