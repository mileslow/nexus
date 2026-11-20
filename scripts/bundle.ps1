# Builds dist\Nexus.exe from the current source, on Windows.
#
#   scripts\bundle.ps1             build Nexus.exe
#   scripts\bundle.ps1 -Package    also write dist\Nexus-Setup.exe (the
#                                  download) and dist\latest-windows.json
#                                  (what the in-app updater fetches)
#
# Packaging needs Inno Setup 6 (`winget install JRSoftware.InnoSetup`).
param([switch]$Package)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

$repo = "mileslow/nexus"
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value

cargo build --release --locked
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\nexus.exe dist\Nexus.exe -Force

$reported = (& dist\Nexus.exe --version | Out-String).Trim()
if ($reported -ne "Nexus $version") {
    throw "dist\Nexus.exe reports '$reported', expected 'Nexus $version'"
}
Write-Host "built dist\Nexus.exe ($version)"

if ($Package) {
    Remove-Item dist\Nexus-Setup.exe, dist\latest-windows.json -ErrorAction SilentlyContinue

    $iscc = @(
        (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source,
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { throw "Inno Setup 6 was not found" }
    & $iscc /Qp "/DAppVersion=$version" scripts\windows\nexus.iss
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # The program on its own, with its checksum, for the updater.
    $sha = (Get-FileHash dist\Nexus.exe -Algorithm SHA256).Hash.ToLower()
    $manifest = @"
{
  "version": "$version",
  "url": "https://github.com/$repo/releases/download/v$version/Nexus.exe",
  "sha256": "$sha"
}
"@
    # Written without a byte-order mark, which the updater would not read.
    [IO.File]::WriteAllText("$PWD\dist\latest-windows.json", $manifest)
    Write-Host "packaged dist\Nexus-Setup.exe, dist\Nexus.exe and dist\latest-windows.json"
}
