@echo off
setlocal
cd /d "%~dp0"
title KARAOKE STUDIO PRO - WASAPI Diagnostic Suite

rem Keep a single launcher implementation. The PowerShell script verifies
rem that the release EXE contains the current embedded Vite frontend.
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0run_diagnostic_ui.ps1"
set "EXIT_CODE=%ERRORLEVEL%"

echo.
echo ============================================================
echo [*] Application process terminated with exit code: %EXIT_CODE%
echo ============================================================
echo.
pause
exit /b %EXIT_CODE%
