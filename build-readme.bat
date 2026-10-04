@echo off
rem ========================================================================
rem  OpenTerm README generator
rem  Rebuilds the codebase-composition section in README.md.
rem ========================================================================
setlocal
set "ROOT=%~dp0"

where powershell.exe >nul 2>nul
if %ERRORLEVEL% NEQ 0 (
    echo [x] PowerShell is required to generate README.md.
    exit /b 1
)

echo [readme] measuring source files ...
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%ROOT%build-readme.ps1"
if %ERRORLEVEL% NEQ 0 (
    echo [x] README generation failed.
    exit /b 1
)

echo [readme] wrote %ROOT%README.md
endlocal
exit /b 0
