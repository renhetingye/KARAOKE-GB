param([switch]$Force)

$ErrorActionPreference = 'Stop'
$modelUrl = 'https://huggingface.co/lj1995/VoiceConversionWebUI/resolve/main/rmvpe.pt?download=true'
$expectedSha256 = '6D62215F4306E3CA278246188607209F09AF3DC77ED4232EFDD069798C4EC193'
$modelsRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot 'models'))
$destination = [System.IO.Path]::GetFullPath((Join-Path $modelsRoot 'rmvpe.pt'))

if (-not $destination.StartsWith($modelsRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing model destination outside analyzer models directory: $destination"
}

if ((Test-Path -LiteralPath $destination) -and -not $Force) {
    $installedHash = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
    if ($installedHash -eq $expectedSha256) {
        Write-Host "RMVPE model is already installed: $destination"
        exit 0
    }
}

New-Item -ItemType Directory -Path $modelsRoot -Force | Out-Null
$temporaryPath = Join-Path ([System.IO.Path]::GetTempPath()) ("karaoke-gb-rmvpe-" + [guid]::NewGuid() + '.pt')
try {
    Write-Host 'Downloading the RMVPE pitch model (about 181 MB)...'
    Invoke-WebRequest -Uri $modelUrl -OutFile $temporaryPath
    $actualHash = (Get-FileHash -LiteralPath $temporaryPath -Algorithm SHA256).Hash
    if ($actualHash -ne $expectedSha256) {
        throw "RMVPE model SHA-256 mismatch. Expected $expectedSha256, got $actualHash"
    }
    Move-Item -LiteralPath $temporaryPath -Destination $destination -Force
    Write-Host "Installed RMVPE model: $destination"
} finally {
    if (Test-Path -LiteralPath $temporaryPath) {
        Remove-Item -LiteralPath $temporaryPath -Force
    }
}
