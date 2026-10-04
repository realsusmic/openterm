@echo off
rem ========================================================================
rem  OpenTerm — Windows build orchestrator
rem  Builds Zig static lib, Go sidecar, and the Rust binary, then stages
rem  everything into dist\regular or dist\portable.
rem
rem  Usage:
rem    build.bat           - regular release build
rem    build.bat regular   - regular release build
rem    build.bat portable  - portable release + openterm.ini beside exe
rem    build.bat debug     - debug build
rem    build.bat clean     - nuke build artifacts
rem    build.bat run       - build release and launch
rem    build.bat small     - release-small profile + UPX pack
rem ========================================================================
setlocal EnableDelayedExpansion
set ROOT=%~dp0
pushd "%ROOT%" >nul

rem --- parse mode ---------------------------------------------------------
set MODE=release
set PACKAGE=regular
if /I "%~1"=="debug" set MODE=debug
if /I "%~1"=="small" set MODE=small
if /I "%~1"=="run"   set MODE=run
if /I "%~1"=="regular"  set MODE=release
if /I "%~1"=="portable" set MODE=release
if /I "%~1"=="portable" set PACKAGE=portable
if /I "%~1"=="clean" goto :clean
set STAGE=dist\%PACKAGE%
set AGENT_OUT=target\otm-agent-%PACKAGE%.exe

echo.
echo [openterm] mode: %MODE%
echo [openterm] package: %PACKAGE%
echo [openterm] root: %ROOT%
echo.

rem --- toolchain checks ---------------------------------------------------
call :need cargo       || goto :missing
call :need rustc       || goto :missing
call :need zig         || goto :missing
call :need go          || goto :missing

rem a C toolchain — MSVC cl.exe (preferred) or clang
where cl.exe >nul 2>nul
if %ERRORLEVEL% NEQ 0 (
    where clang.exe >nul 2>nul
    if !ERRORLEVEL! NEQ 0 (
        echo [x] no C compiler found. Install VS Build Tools or LLVM/clang.
        goto :fail
    )
)

rem --- staging dirs -------------------------------------------------------
if not exist "target\native" mkdir "target\native"
if not exist "%STAGE%"       mkdir "%STAGE%"

rem ========================================================================
rem  1) Zig: build fastgrid static lib
rem ========================================================================
echo [1/3] zig  : building fastgrid.lib ...
set ZIG_OPT=-O ReleaseFast
if "%MODE%"=="debug" set ZIG_OPT=-O Debug
pushd native >nul
zig build-lib fastgrid.zig %ZIG_OPT% -target x86_64-windows-msvc -femit-bin="..\target\native\fastgrid.lib" 2>&1
if %ERRORLEVEL% NEQ 0 ( popd & goto :fail )
popd >nul

rem clean up the loose .obj zig leaves next to the source
del /q native\fastgrid.obj  2>nul
del /q "target\native\fastgrid.lib.obj" 2>nul
del /q native\fastgrid.lib  2>nul

if not exist "target\native\fastgrid.lib" (
    echo [x] zig: output missing after build
    goto :fail
)
echo       ok.

rem ========================================================================
rem  2) Go: build otm-agent sidecar
rem ========================================================================
echo [2/3] go   : building otm-agent.exe ...
pushd agent >nul
if not exist "go.sum" (
    echo       go mod download ...
    go mod download 2>&1
    if !ERRORLEVEL! NEQ 0 ( popd & goto :fail )
)
set GOFLAGS=-trimpath
set CGO_ENABLED=0
set GO_LDFLAGS=-s -w
if "%MODE%"=="debug" set GO_LDFLAGS=
go build -ldflags "%GO_LDFLAGS%" -o "..\%AGENT_OUT%" .
if %ERRORLEVEL% NEQ 0 ( popd & goto :fail )
popd >nul
echo       ok.

rem ========================================================================
rem  3) Rust: build main binary (invokes build.rs -> cc for vtparse.c,
rem                              links target\native\fastgrid.lib)
rem ========================================================================
echo [3/3] rust : building openterm.exe ...
set CARGO_FEATURE=
if "%PACKAGE%"=="portable" set CARGO_FEATURE=--features portable
if "%MODE%"=="debug" (
    cargo build %CARGO_FEATURE%
    set OUT_DIR=target\debug
) else if "%MODE%"=="small" (
    cargo build --profile release-small %CARGO_FEATURE%
    set OUT_DIR=target\release-small
) else (
    cargo build --release %CARGO_FEATURE%
    set OUT_DIR=target\release
)
if %ERRORLEVEL% NEQ 0 goto :fail

copy /y "%OUT_DIR%\openterm.exe" "%STAGE%\openterm.exe" >nul
if %ERRORLEVEL% NEQ 0 goto :fail
copy /y "%AGENT_OUT%" "%STAGE%\otm-agent.exe" >nul
if %ERRORLEVEL% NEQ 0 goto :fail
echo       ok.

if "%PACKAGE%"=="portable" (
    > "%STAGE%\openterm.ini" (
        echo ; OpenTerm settings
        echo [appearance]
        echo theme=dark
        echo.
        echo [terminal]
        echo default_shell=powershell
        echo.
        echo [vault]
        echo unlock_grace_value=1
        echo unlock_grace_unit=day
    )
)

rem --- optional UPX pass --------------------------------------------------
if "%MODE%"=="small" (
    where upx.exe >nul 2>nul
    if !ERRORLEVEL! EQU 0 (
        echo [+] upx : compressing binaries ...
        upx --best --lzma "%STAGE%\openterm.exe"   >nul 2>nul
        upx --best --lzma "%STAGE%\otm-agent.exe"  >nul 2>nul
    ) else (
        echo [i] upx not found, skipping compression step.
    )
)

echo.
echo ====================================================
echo  openterm built:  %ROOT%%STAGE%\openterm.exe
echo  otm-agent:       %ROOT%%STAGE%\otm-agent.exe
if "%PACKAGE%"=="portable" echo  settings:        %ROOT%%STAGE%\openterm.ini
echo ====================================================
for %%f in ("%STAGE%\openterm.exe" "%STAGE%\otm-agent.exe") do (
    for %%A in (%%f) do echo   %%~zA bytes   %%~nxA
)
echo.

if "%MODE%"=="run" (
    echo [run] launching ...
    start "" "%STAGE%\openterm.exe"
)

popd >nul
endlocal
exit /b 0

rem ========================================================================
:need
where %1.exe >nul 2>nul
if %ERRORLEVEL% NEQ 0 (
    where %1 >nul 2>nul
    if !ERRORLEVEL! NEQ 0 (
        echo [x] missing on PATH: %1
        exit /b 1
    )
)
exit /b 0

:missing
echo.
echo required toolchains:
echo   rust  : https://rustup.rs
echo   zig   : https://ziglang.org/download/     ^(0.13+^)
echo   go    : https://go.dev/dl/                 ^(1.22+^)
echo   C     : Visual Studio Build Tools (MSVC)   OR LLVM/clang
goto :fail

:clean
echo [clean] removing build artifacts ...
if exist "target"  rmdir /s /q "target"
if exist "dist"    rmdir /s /q "dist"
if exist "agent\otm-agent.exe" del /q "agent\otm-agent.exe"
echo [clean] done.
popd >nul
endlocal
exit /b 0

:fail
echo.
echo [x] build failed.
popd >nul
endlocal
exit /b 1
