param(
    [string]$Version = 'dev',
    [string]$OutputDirectory = '',
    [switch]$SkipBuild,
    [switch]$SkipModelDownload
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$releaseRoot = if ($OutputDirectory) { [System.IO.Path]::GetFullPath($OutputDirectory) } else { Join-Path $projectRoot 'release' }
$stageRoot = [System.IO.Path]::GetFullPath((Join-Path $releaseRoot 'KARAOKE-GB-portable'))
$safeVersion = $Version -replace '[^A-Za-z0-9._-]', '-'
$zipPath = [System.IO.Path]::GetFullPath((Join-Path $releaseRoot "KARAOKE-GB-$safeVersion-windows-x64-portable.zip"))

if (-not $stageRoot.StartsWith([System.IO.Path]::GetFullPath($releaseRoot), [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing staging path outside release directory: $stageRoot"
}

if (-not $SkipBuild) {
    & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File (Join-Path $projectRoot 'build_release.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
}

if (-not $SkipModelDownload) {
    foreach ($installer in 'install_game_model.ps1', 'install_rmvpe_model.ps1') {
        & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File (Join-Path $projectRoot "services\analyzer\$installer")
        if ($LASTEXITCODE -ne 0) { throw "$installer failed." }
    }
}

$releaseExe = Join-Path $projectRoot 'target\release\KARAOKE-GB.exe'
if (-not (Test-Path -LiteralPath $releaseExe)) { throw "Missing release executable: $releaseExe" }

New-Item -ItemType Directory -Path $releaseRoot -Force | Out-Null
if (Test-Path -LiteralPath $stageRoot) { Remove-Item -LiteralPath $stageRoot -Recurse -Force }
New-Item -ItemType Directory -Path $stageRoot | Out-Null

Copy-Item -LiteralPath $releaseExe -Destination (Join-Path $stageRoot 'KARAOKE-GB.exe')
Copy-Item -LiteralPath (Join-Path $projectRoot 'START_KARAOKE-GB.bat') -Destination $stageRoot
Copy-Item -LiteralPath (Join-Path $projectRoot 'run_diagnostic_ui.ps1') -Destination $stageRoot
Copy-Item -LiteralPath (Join-Path $projectRoot 'packaging\README_PORTABLE.txt') -Destination (Join-Path $stageRoot 'README.txt')
Copy-Item -LiteralPath (Join-Path $projectRoot 'packaging\portable.manifest.json') -Destination $stageRoot
Copy-Item -LiteralPath (Join-Path $projectRoot 'schemas') -Destination $stageRoot -Recurse

$analyzerDestination = Join-Path $stageRoot 'services\analyzer'
New-Item -ItemType Directory -Path $analyzerDestination -Force | Out-Null
foreach ($file in 'requirements.txt', 'pyproject.toml', 'install_game_model.ps1', 'install_rmvpe_model.ps1', 'THIRD_PARTY.md') {
    Copy-Item -LiteralPath (Join-Path $projectRoot "services\analyzer\$file") -Destination $analyzerDestination
}
$analyzerSourceDestination = Join-Path $analyzerDestination 'src'
New-Item -ItemType Directory -Path $analyzerSourceDestination -Force | Out-Null
Get-ChildItem -LiteralPath (Join-Path $projectRoot 'services\analyzer\src') -File -Filter '*.py' |
    ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $analyzerSourceDestination }
if (Test-Path -LiteralPath (Join-Path $projectRoot 'services\analyzer\models')) {
    Copy-Item -LiteralPath (Join-Path $projectRoot 'services\analyzer\models') -Destination $analyzerDestination -Recurse
}

foreach ($folder in 'library', 'exports', 'recordings', 'logs') {
    New-Item -ItemType Directory -Path (Join-Path $stageRoot $folder) -Force | Out-Null
}
[System.IO.File]::WriteAllText(
    (Join-Path $stageRoot 'library\songs.json'),
    '[]',
    [System.Text.UTF8Encoding]::new($false)
)

if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
Compress-Archive -LiteralPath $stageRoot -DestinationPath $zipPath -CompressionLevel Optimal
$hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
$hashPath = "$zipPath.sha256.txt"
Set-Content -LiteralPath $hashPath -Value "$hash  $(Split-Path -Leaf $zipPath)" -Encoding ASCII

Write-Host "[+] Portable ZIP: $zipPath" -ForegroundColor Green
Write-Host "[+] SHA-256: $hash" -ForegroundColor Green
