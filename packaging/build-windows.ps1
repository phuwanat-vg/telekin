# Build the Windows installer for the Telekin viewer.
#
#   powershell -File packaging\build-windows.ps1
#
# Needs Inno Setup 6 (winget install JRSoftware.InnoSetup). Output: dist\.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$version = (Select-String -Path "Cargo.toml" -Pattern '^version = "([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw "could not read the workspace version from Cargo.toml" }

$iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found; run: winget install JRSoftware.InnoSetup" }

Write-Host "== telekin $version (windows x64) =="
cargo build --release -p telekin
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

New-Item -ItemType Directory -Force -Path "dist" | Out-Null
& $iscc "/DAppVersion=$version" "/Q" "packaging\windows\telekin.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

Write-Host ""
Write-Host "built:"
Get-ChildItem "dist\telekin-$version-windows-x64-setup.exe" | ForEach-Object { "  $($_.FullName)  ($([math]::Round($_.Length / 1MB, 1)) MB)" }
