# tools/whisper-smoke.ps1 — wrapper for the M5b W4c smoke tests.
#
# Sets the cmake/llvm PATH that the whisper-rs build script
# needs + the model-path env var the smoke tests look at, then
# delegates to `cargo test --features stt-whisper --test
# whisper_smoke -- --ignored`. Runs from any directory under
# the repo root.
#
# Use:
#
#   .\tools\whisper-smoke.ps1                # default model
#   .\tools\whisper-smoke.ps1 -Model X.bin   # alternate model
#   .\tools\whisper-smoke.ps1 -Show          # passes --nocapture so
#                                            # whisper.cpp's chatter
#                                            # is visible (avoids the
#                                            # PowerShell-reserved
#                                            # `Verbose` name)
#
# First run takes ~3 minutes (whisper.cpp compiles from source);
# subsequent runs are <5 seconds (cargo cache).

[CmdletBinding()]
param(
    [string]$Model = "$env:USERPROFILE\.iot-athome\models\ggml-base.en.bin",
    [switch]$Show
)

$ErrorActionPreference = "Stop"
# This script lives at <repo>/tools/whisper-smoke.ps1 — one
# `Split-Path -Parent` to climb out of `tools/`.
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

# Confirm the model file exists. The smoke tests self-skip
# without `IOT_WHISPER_MODEL_PATH` set, so a missing file is
# the most likely user-error path.
if (-not (Test-Path $Model)) {
    Write-Host "[whisper-smoke] model not found at $Model" -ForegroundColor Red
    Write-Host "" -ForegroundColor Red
    Write-Host "  Download once with:" -ForegroundColor Yellow
    Write-Host "    `$dir = `"`$env:USERPROFILE\.iot-athome\models`"" -ForegroundColor Yellow
    Write-Host "    New-Item -ItemType Directory -Force `$dir | Out-Null" -ForegroundColor Yellow
    Write-Host "    Invoke-WebRequest -OutFile `"`$dir\ggml-base.en.bin`" ``" -ForegroundColor Yellow
    Write-Host "      -Uri `"https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin`"" -ForegroundColor Yellow
    exit 1
}

# Prepend the tools we need to PATH for this process so the
# wrapper works regardless of how the parent shell was
# launched. Native PowerShell from Start menu has these on
# PATH already (per the User-scope env); but invocation from
# Git Bash / WSL bash / a `cmd` window may not inherit them.
# Belt-and-braces.
$cmakeBin = "C:\Program Files\CMake\bin"           # whisper-rs build script
$llvmBin  = "C:\Program Files\LLVM\bin"            # bindgen needs clang
$cargoBin = "$env:USERPROFILE\.cargo\bin"          # rustup-installed cargo
foreach ($dir in @($cmakeBin, $llvmBin, $cargoBin)) {
    if (-not (Test-Path "$dir\")) {
        Write-Host "[whisper-smoke] missing tool directory: $dir" -ForegroundColor Red
        if ($dir -eq $cargoBin) {
            Write-Host "  install rust: https://rustup.rs" -ForegroundColor Yellow
        } else {
            Write-Host "  install with:  choco install cmake llvm" -ForegroundColor Yellow
            Write-Host "  (run from an elevated PowerShell)" -ForegroundColor Yellow
        }
        exit 1
    }
}
$env:PATH = "$cmakeBin;$llvmBin;$cargoBin;$env:PATH"
$env:IOT_WHISPER_MODEL_PATH = $Model

Write-Host "[whisper-smoke] model: $Model" -ForegroundColor Cyan
Write-Host "[whisper-smoke] cmake: $((Get-Command cmake).Source)" -ForegroundColor Cyan
Write-Host "[whisper-smoke] clang: $((Get-Command clang).Source)" -ForegroundColor Cyan
Write-Host "[whisper-smoke] cargo: $((Get-Command cargo).Source)" -ForegroundColor Cyan
Write-Host "" -ForegroundColor Cyan

$cargoArgs = @(
    "test", "--features", "stt-whisper",
    "--test", "whisper_smoke",
    "--", "--ignored"
)
if ($Show) {
    $cargoArgs += "--nocapture"
}

& cargo @cargoArgs
exit $LASTEXITCODE
