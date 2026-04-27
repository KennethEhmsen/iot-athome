# tools/listen-smoke.ps1 — speak-and-watch wrapper for the
# M5b W4c+W4a real-mic + whisper STT path on Windows.
#
# Runs `iot-voice-daemon listen --use-mic --stt-model <path>
# --dry-run` with cmake/llvm/cargo/IOT_WHISPER_MODEL_PATH all
# pre-staged so it works from any shell. `--dry-run` skips the
# NATS bus connection — the daemon prints transcribed intents
# to its log instead of publishing. Useful to validate mic
# capture + wake detection + whisper STT *before* bringing
# up the dev stack.
#
# Use:
#
#   .\tools\listen-smoke.ps1                  # default model
#   .\tools\listen-smoke.ps1 -Model X.bin     # alternate model
#   .\tools\listen-smoke.ps1 -Release         # opt-release build
#                                              # (whisper inference
#                                              #  is slower in dev)
#
# What you'll see when it's working:
#
#   [whisper-smoke] cmake / clang / cargo paths printed up top
#   info: starting cpal audio capture; speak after startup
#   info: iot-voice listen --dry-run: bus skipped; intents log-only
#   <speak something into your mic>
#   info: intent (log-sink only; bus publish not wired)
#     domain=lights verb=on raw="turn on the kitchen light"
#
# Ctrl+C to stop.

[CmdletBinding()]
param(
    [string]$Model = "$env:USERPROFILE\.iot-athome\models\ggml-base.en.bin",
    [switch]$Release
)

$ErrorActionPreference = "Stop"
# This script lives at <repo>/tools/listen-smoke.ps1.
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if (-not (Test-Path $Model)) {
    Write-Host "[listen-smoke] model not found at $Model" -ForegroundColor Red
    Write-Host "" -ForegroundColor Red
    Write-Host "  Download once with:" -ForegroundColor Yellow
    Write-Host "    `$dir = `"`$env:USERPROFILE\.iot-athome\models`"" -ForegroundColor Yellow
    Write-Host "    New-Item -ItemType Directory -Force `$dir | Out-Null" -ForegroundColor Yellow
    Write-Host "    Invoke-WebRequest -OutFile `"`$dir\ggml-base.en.bin`" ``" -ForegroundColor Yellow
    Write-Host "      -Uri `"https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin`"" -ForegroundColor Yellow
    exit 1
}

$cmakeBin = "C:\Program Files\CMake\bin"
$llvmBin  = "C:\Program Files\LLVM\bin"
$cargoBin = "$env:USERPROFILE\.cargo\bin"
foreach ($dir in @($cmakeBin, $llvmBin, $cargoBin)) {
    if (-not (Test-Path "$dir\")) {
        Write-Host "[listen-smoke] missing tool directory: $dir" -ForegroundColor Red
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

Write-Host "[listen-smoke] model: $Model" -ForegroundColor Cyan
Write-Host "[listen-smoke] cargo: $((Get-Command cargo).Source)" -ForegroundColor Cyan
Write-Host "" -ForegroundColor Cyan

# Build args: cargo run -p iot-voice-daemon --features mic,stt-whisper [--release]
#   -- listen --use-mic --stt-model <model> --dry-run
$cargoArgs = @(
    "run", "-p", "iot-voice-daemon",
    "--features", "mic,stt-whisper"
)
if ($Release) {
    $cargoArgs += "--release"
}
$cargoArgs += @(
    "--",
    "listen",
    "--use-mic",
    "--stt-model", $Model,
    "--dry-run"
)

Write-Host "[listen-smoke] starting daemon (Ctrl+C to stop)..." -ForegroundColor Green
Write-Host "  Speak something after the cpal capture log line." -ForegroundColor Green
Write-Host "" -ForegroundColor Green

& cargo @cargoArgs
exit $LASTEXITCODE
