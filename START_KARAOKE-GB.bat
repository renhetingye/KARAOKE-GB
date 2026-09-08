@echo off
setlocal
cd /d "%~dp0"
title KARAOKE-GB - First-run Setup and Launcher

powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0run_diagnostic_ui.ps1"
set "EXIT_CODE=%ERRORLEVEL%"

echo.
echo ============================================================
echo [*] KARAOKE-GB finished with exit code: %EXIT_CODE%
echo ============================================================
echo.
pause
exit /b %EXIT_CODE%
