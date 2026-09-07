param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$modelName = 'GAME-1.0.3-small-onnx'
$archiveUrl = 'https://github.com/openvpi/GAME/releases/download/v1.0.3/GAME-1.0.3-small-onnx.zip'
$archiveSha256 = '00BA0C64115B6B874D9EA4AFD3E6CF822ABDA2A04E52569233B0A044FD40E4E8'
$modelsRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot 'models'))
$destination = [System.IO.Path]::GetFullPath((Join-Path $modelsRoot 'game-1.0.3-small-onnx'))

if (-not $destination.StartsWith($modelsRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing model destination outside analyzer models directory: $destination"
}

$required = 'encoder.onnx', 'segmenter.onnx', 'estimator.onnx', 'bd2dur.onnx', 'config.json'
$complete = $required | ForEach-Object { Test-Path -LiteralPath (Join-Path $destination $_) }
if (($complete -notcontains $false) -and -not $Force) {
    Write-Host "GAME 1.0.3 small ONNX is already installed: $destination"
    exit 0
}

$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("karaoke-game-model-" + [guid]::NewGuid())
$archivePath = Join-Path $temporaryRoot 'game.zip'
$expandedPath = Join-Path $temporaryRoot 'expanded'
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null

try {
    Write-Host "Downloading official OpenVPI GAME 1.0.3 small ONNX model..."
    Invoke-WebRequest -Uri $archiveUrl -OutFile $archivePath
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash
    if ($actualHash -ne $archiveSha256) {
        throw "GAME archive SHA-256 mismatch. Expected $archiveSha256, got $actualHash"
    }

    Expand-Archive -LiteralPath $archivePath -DestinationPath $expandedPath
    $source = Join-Path $expandedPath $modelName
    foreach ($file in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $source $file))) {
            throw "GAME archive is missing required file: $file"
        }
    }

    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    foreach ($file in Get-ChildItem -LiteralPath $source -File) {
        Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $destination $file.Name) -Force
    }
    Write-Host "Installed GAME model: $destination"
} finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}
