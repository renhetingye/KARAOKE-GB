# KARAOKE STUDIO PRO launch script
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location -LiteralPath $projectRoot

Write-Host '============================================================' -ForegroundColor Cyan
Write-Host '  KARAOKE STUDIO PRO - WASAPI Diagnostic Suite' -ForegroundColor Cyan
Write-Host '============================================================' -ForegroundColor Cyan

$executablePath = Join-Path $projectRoot 'target\release\appsdesktop.exe'
if (-not (Test-Path -LiteralPath $executablePath)) {
    $executablePath = Join-Path $projectRoot 'target\debug\appsdesktop.exe'
}
if (-not (Test-Path -LiteralPath $executablePath)) {
    Write-Host "[ERROR] Binary not found: $executablePath" -ForegroundColor Red
    exit 1
}

# A plain `cargo build --release` compiles Tauri with its devUrl and does not
# embed the Vite assets. Refuse that binary instead of opening a misleading
# localhost/ERR_CONNECTION_REFUSED page.
$distIndexPath = Join-Path $projectRoot 'apps\desktop\dist\index.html'
if ((Split-Path -Leaf (Split-Path -Parent $executablePath)) -eq 'release' -and
    (Test-Path -LiteralPath $distIndexPath)) {
    $distIndex = Get-Content -LiteralPath $distIndexPath -Raw -Encoding UTF8
    $assetMatch = [regex]::Match($distIndex, 'src="/assets/([^"?]+\.js)')
    if ($assetMatch.Success) {
        $expectedAssetName = $assetMatch.Groups[1].Value
        $binaryText = [System.Text.Encoding]::ASCII.GetString(
            [System.IO.File]::ReadAllBytes($executablePath)
        )
        if (-not $binaryText.Contains($expectedAssetName)) {
            Write-Host '[ERROR] This release EXE does not contain the current frontend assets.' -ForegroundColor Red
            Write-Host '[ERROR] Do not use plain cargo build for the desktop release.' -ForegroundColor Red
            Write-Host '[*] Run .\build_release.ps1, then start this launcher again.' -ForegroundColor Yellow
            exit 2
        }
    }
}

Write-Host "[*] Starting: $executablePath" -ForegroundColor Green
& $executablePath
$processExitCode = $LASTEXITCODE
Write-Host "[*] Process finished with exit code: $processExitCode" -ForegroundColor Yellow
exit $processExitCode
