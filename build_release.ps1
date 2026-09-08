# Canonical desktop release builder. This must be used instead of a direct
# `cargo build --release` so Tauri embeds the current Vite frontend.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$desktopRoot = Join-Path $projectRoot 'apps\desktop'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

$npmCommand = Get-Command npm.cmd -ErrorAction SilentlyContinue
if ($npmCommand) {
    $npmPath = $npmCommand.Source
} else {
    $runtimeRoot = Join-Path $env:LOCALAPPDATA 'OpenAI\Codex\runtimes\cua_node'
    $npmPath = Get-ChildItem -LiteralPath $runtimeRoot -Recurse -Filter npm.cmd -ErrorAction SilentlyContinue |
        Where-Object { $_.DirectoryName -match '\\bin$' } |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}

if (-not $npmPath -or -not (Test-Path -LiteralPath $npmPath)) {
    throw 'npm.cmd was not found. Install Node.js or make npm.cmd available on PATH.'
}

$nodeBin = Split-Path -Parent $npmPath
$env:PATH = "$nodeBin;$cargoBin;$env:PATH"
Set-Location -LiteralPath $desktopRoot

Write-Host '[*] Building Vite frontend and Tauri release executable...' -ForegroundColor Cyan
& $npmPath run tauri -- build --no-bundle
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$executablePath = Join-Path $projectRoot 'target\release\appsdesktop.exe'
$brandedExecutablePath = Join-Path $projectRoot 'target\release\KARAOKE-GB.exe'
Copy-Item -LiteralPath $executablePath -Destination $brandedExecutablePath -Force
$hash = Get-FileHash -LiteralPath $executablePath -Algorithm SHA256
Write-Host "[+] Release ready: $brandedExecutablePath" -ForegroundColor Green
Write-Host "[+] SHA-256: $($hash.Hash)" -ForegroundColor Green
