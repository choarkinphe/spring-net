$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
if ($env:OS -ne 'Windows_NT') { throw 'Please build the Windows installer on Windows 10/11 with MSVC Build Tools and WebView2.' }
if (-not (Test-Path (Join-Path $Root 'node_modules/.bin/tauri.cmd'))) { throw 'Project Tauri CLI is missing. Run npm ci in the project directory first.' }
Push-Location $Root
try {
    & node (Join-Path $Root 'scripts/version.cjs') --check
    if ($LASTEXITCODE -ne 0) { throw 'Version consistency check failed.' }
    & npm.cmd run desktop:build:windows -- --ci -- --locked
    if ($LASTEXITCODE -ne 0) { throw 'Tauri Windows build failed.' }
    $Installers = @(Get-ChildItem (Join-Path $Root 'src-tauri/target/release/bundle/nsis/spring-net_*-setup.exe'))
    if ($Installers.Count -eq 0) { throw 'No NSIS installer was generated.' }
    $Dist = Join-Path $Root 'dist'
    New-Item -ItemType Directory -Force $Dist | Out-Null
    foreach ($Installer in $Installers) {
        $Destination = Join-Path $Dist $Installer.Name
        if (Test-Path $Destination) { Move-Item $Destination ($Destination + '.previous.' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
        Copy-Item $Installer.FullName $Destination
        Write-Host "Windows installer: $Destination (unsigned; validate on Windows before distribution)"
    }
} finally { Pop-Location }
