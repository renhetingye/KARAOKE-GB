param(
    [switch]$SkipSetup,
    [switch]$Rebuild
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$logsRoot = Join-Path $projectRoot 'logs'
$portableMarker = Join-Path $projectRoot 'portable.manifest.json'
$isPortable = Test-Path -LiteralPath $portableMarker
New-Item -ItemType Directory -Path $logsRoot -Force | Out-Null
$logPath = Join-Path $logsRoot ("startup-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
$transcriptStarted = $false

function Write-Step([string]$Message) {
    Write-Host "[*] $Message" -ForegroundColor Cyan
}

function Refresh-ProcessPath {
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:PATH = "$machinePath;$userPath"
}

function Install-WingetPackage([string]$Id, [string]$Label) {
    $winget = Get-Command winget.exe -ErrorAction SilentlyContinue
    if (-not $winget) { return $false }
    Write-Step "Installing $Label..."
    & $winget.Source install --id $Id --exact --accept-package-agreements --accept-source-agreements --silent | Out-Host
    if ($LASTEXITCODE -ne 0) { return $false }
    Refresh-ProcessPath
    return $true
}

function Get-Python311 {
    $py = Get-Command py.exe -ErrorAction SilentlyContinue
    if ($py) {
        & $py.Source -3.11 -c 'import sys' 2>$null
        if ($LASTEXITCODE -eq 0) { return ,@($py.Source, '-3.11') }
    }
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python311\python.exe'),
        (Join-Path $env:ProgramFiles 'Python311\python.exe')
    )
    $python = Get-Command python.exe -ErrorAction SilentlyContinue
    if ($python) { $candidates = @($python.Source) + $candidates }
    foreach ($candidate in $candidates | Select-Object -Unique) {
        if (-not (Test-Path -LiteralPath $candidate)) { continue }
        & $candidate -c 'import sys; raise SystemExit(0 if sys.version_info[:2] == (3, 11) else 1)' 2>$null
        if ($LASTEXITCODE -eq 0) { return ,@($candidate) }
    }
    return $null
}

function Ensure-Python311 {
    $pythonCommand = Get-Python311
    if ($pythonCommand) { return $pythonCommand }
    if (Install-WingetPackage 'Python.Python.3.11' 'Python 3.11') {
        $pythonCommand = Get-Python311
        if ($pythonCommand) { return $pythonCommand }
    }

    Write-Step 'winget is unavailable or failed; downloading the official Python 3.11.9 installer...'
    $installer = Join-Path ([System.IO.Path]::GetTempPath()) ("python-3.11.9-" + [guid]::NewGuid() + '.exe')
    try {
        Invoke-WebRequest -Uri 'https://www.python.org/ftp/python/3.11.9/python-3.11.9-amd64.exe' -OutFile $installer
        $signature = Get-AuthenticodeSignature -LiteralPath $installer
        if ($signature.Status -ne 'Valid') { throw "Python installer signature is not valid: $($signature.Status)" }
        $process = Start-Process -FilePath $installer -ArgumentList '/quiet InstallAllUsers=0 PrependPath=1 Include_test=0 Include_launcher=1' -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "Python installer failed with exit code $($process.ExitCode)." }
    } finally {
        if (Test-Path -LiteralPath $installer) { Remove-Item -LiteralPath $installer -Force }
    }
    Refresh-ProcessPath
    $pythonCommand = Get-Python311
    if (-not $pythonCommand) { throw 'Python 3.11 was installed but could not be located.' }
    return $pythonCommand
}

function Test-WebView2Installed {
    $clientId = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    $locations = @(
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$clientId",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$clientId",
        "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$clientId"
    )
    foreach ($location in $locations) {
        if (Test-Path $location) {
            $version = (Get-ItemProperty -Path $location -Name 'pv' -ErrorAction SilentlyContinue).pv
            if ($version -and $version -ne '0.0.0.0') { return $true }
        }
    }
    $runtime = Get-ChildItem 'C:\Program Files (x86)\Microsoft\EdgeWebView\Application' -Filter msedgewebview2.exe -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
    return $null -ne $runtime
}

function Ensure-WebView2 {
    if (Test-WebView2Installed) { return }
    if (Install-WingetPackage 'Microsoft.EdgeWebView2Runtime' 'Microsoft WebView2 Runtime') { return }
    Write-Step 'Downloading the official Microsoft WebView2 bootstrapper...'
    $installer = Join-Path ([System.IO.Path]::GetTempPath()) ("webview2-" + [guid]::NewGuid() + '.exe')
    try {
        Invoke-WebRequest -Uri 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile $installer
        $signature = Get-AuthenticodeSignature -LiteralPath $installer
        if ($signature.Status -ne 'Valid') { throw "WebView2 installer signature is not valid: $($signature.Status)" }
        $process = Start-Process -FilePath $installer -ArgumentList '/silent /install' -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "WebView2 installer failed with exit code $($process.ExitCode)." }
    } finally {
        if (Test-Path -LiteralPath $installer) { Remove-Item -LiteralPath $installer -Force }
    }
}

function Ensure-AnalyzerEnvironment {
    $analyzerRoot = Join-Path $projectRoot 'services\analyzer'
    $venvRoot = Join-Path $analyzerRoot '.venv'
    $venvPython = Join-Path $venvRoot 'Scripts\python.exe'
    $requirements = Join-Path $analyzerRoot 'requirements.txt'
    $stamp = Join-Path $venvRoot '.karaoke-gb-ready'
    if (-not (Test-Path -LiteralPath $requirements)) { throw "Missing analyzer requirements: $requirements" }
    $requirementsHash = (Get-FileHash -LiteralPath $requirements -Algorithm SHA256).Hash

    if (-not (Test-Path -LiteralPath $venvPython)) {
        $pythonCommand = Ensure-Python311
        Write-Step 'Creating the private Python environment...'
        $pythonExe = $pythonCommand[0]
        $pythonArgs = @($pythonCommand | Select-Object -Skip 1)
        & $pythonExe @pythonArgs -m venv $venvRoot
        if ($LASTEXITCODE -ne 0) { throw 'Python virtual environment creation failed.' }
    }

    $installedHash = if (Test-Path -LiteralPath $stamp) { (Get-Content -LiteralPath $stamp -Raw).Trim() } else { '' }
    if ($installedHash -ne $requirementsHash) {
        & $venvPython -m pip --version *> $null
        if ($LASTEXITCODE -ne 0) {
            Write-Step 'pip is missing; repairing the Python environment...'
            & $venvPython -m ensurepip --upgrade
            if ($LASTEXITCODE -ne 0) { throw 'Automatic pip repair failed.' }
        }
        Write-Step 'Installing Demucs and audio analysis packages. The first run can take a long time...'
        & $venvPython -m pip install --disable-pip-version-check --upgrade pip
        if ($LASTEXITCODE -ne 0) { throw 'pip upgrade failed.' }
        & $venvPython -m pip install --disable-pip-version-check -r $requirements
        if ($LASTEXITCODE -ne 0) { throw 'Python/Demucs dependency installation failed.' }
        Set-Content -LiteralPath $stamp -Value $requirementsHash -Encoding ASCII
    }

    foreach ($modelInstaller in 'install_game_model.ps1', 'install_rmvpe_model.ps1') {
        $installerPath = Join-Path $analyzerRoot $modelInstaller
        if (-not (Test-Path -LiteralPath $installerPath)) { throw "Missing model installer: $installerPath" }
        & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File $installerPath
        if ($LASTEXITCODE -ne 0) { throw "$modelInstaller failed." }
    }
}

function Ensure-ReleaseBinary {
    $candidates = @(
        (Join-Path $projectRoot 'KARAOKE-GB.exe'),
        (Join-Path $projectRoot 'target\release\KARAOKE-GB.exe'),
        (Join-Path $projectRoot 'target\release\appsdesktop.exe')
    )
    if (-not $Rebuild) {
        foreach ($candidate in $candidates) {
            if (Test-Path -LiteralPath $candidate) { return $candidate }
        }
    }
    if ($isPortable) { throw 'KARAOKE-GB.exe is missing from the portable package. Extract the ZIP again.' }
    if (-not (Get-Command npm.cmd -ErrorAction SilentlyContinue)) {
        if (-not (Install-WingetPackage 'OpenJS.NodeJS.LTS' 'Node.js LTS')) { throw 'Node.js LTS is required to rebuild from source.' }
    }
    if (-not (Get-Command cargo.exe -ErrorAction SilentlyContinue)) {
        if (-not (Install-WingetPackage 'Rustlang.Rustup' 'Rust')) { throw 'Rust is required to rebuild from source.' }
        $env:PATH = "$(Join-Path $env:USERPROFILE '.cargo\bin');$env:PATH"
    }
    Write-Step 'Building KARAOKE-GB...'
    & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File (Join-Path $projectRoot 'build_release.ps1') | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Application build failed. See the log for details.' }
    $result = Join-Path $projectRoot 'target\release\KARAOKE-GB.exe'
    if (-not (Test-Path -LiteralPath $result)) { throw 'The release executable was not created.' }
    return $result
}

Set-Location -LiteralPath $projectRoot
try {
    Start-Transcript -LiteralPath $logPath -Force | Out-Null
    $transcriptStarted = $true
    Write-Host '============================================================' -ForegroundColor Cyan
    Write-Host ' KARAOKE-GB - Setup and Diagnostic Launcher' -ForegroundColor Cyan
    Write-Host '============================================================' -ForegroundColor Cyan
    Write-Host "Log: $logPath" -ForegroundColor DarkGray
    if (-not $SkipSetup) {
        Ensure-WebView2
        Ensure-AnalyzerEnvironment
    }
    foreach ($folder in 'library', 'exports', 'recordings') {
        New-Item -ItemType Directory -Path (Join-Path $projectRoot $folder) -Force | Out-Null
    }
    $env:KARAOKE_GB_ROOT = $projectRoot
    $executablePath = Ensure-ReleaseBinary
    Write-Step "Starting $executablePath"
    & $executablePath
    $processExitCode = if ($null -eq $LASTEXITCODE) { 0 } else { $LASTEXITCODE }
    Write-Host "[*] Process finished with exit code: $processExitCode" -ForegroundColor Yellow
    if ($processExitCode -ne 0) { throw "Application exited with code $processExitCode." }
} catch {
    Write-Host "[ERROR] $($_.Exception.Message)" -ForegroundColor Red
    Write-Host "[ERROR] Diagnostic log: $logPath" -ForegroundColor Red
    Read-Host 'Press Enter to close'
    exit 1
} finally {
    if ($transcriptStarted) { Stop-Transcript | Out-Null }
}
