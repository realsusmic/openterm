# Run from any directory. Extra arguments are passed directly to libFuzzer.
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $FuzzerArgs
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$originalPath = $env:Path
$exitCode = 1

try {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw 'Visual Studio Build Tools with the C++ AddressSanitizer component is required.'
    }
    $vsRoot = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vsRoot) {
        throw 'No Visual Studio C++ Build Tools installation was found.'
    }
    $versionFile = Join-Path $vsRoot 'VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt'
    $version = (Get-Content -LiteralPath $versionFile -Raw).Trim()
    $runtimeDir = Join-Path $vsRoot "VC\Tools\MSVC\$version\bin\Hostx64\x64"
    if (-not (Test-Path -LiteralPath (Join-Path $runtimeDir 'clang_rt.asan_dynamic-x86_64.dll'))) {
        throw 'Install the C++ AddressSanitizer component in Visual Studio Installer.'
    }
    $llvmBin = Join-Path $env:ProgramFiles 'LLVM\bin'
    if (-not (Test-Path -LiteralPath (Join-Path $llvmBin 'clang-cl.exe'))) {
        throw 'LLVM clang-cl is required to instrument the C parser.'
    }
    # Microsoft libFuzzer objects import ASan entry points absent from the
    # upstream LLVM DLL, even when both runtimes have the same LLVM version.
    $env:Path = "$runtimeDir;$llvmBin;$originalPath"
    $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    Push-Location -LiteralPath $repoRoot
    try {
        & $cargo +nightly fuzz run vt_parser -- '-dict=fuzz/dictionaries/vt_parser.dict' @FuzzerArgs
        $exitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
} finally {
    $env:Path = $originalPath
}
exit $exitCode
