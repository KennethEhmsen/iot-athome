# tools/setup-whisper-env.ps1 — one-time persistent env setup
# for the M5b W4c whisper STT path on Windows.
#
# Updates the User-scope PATH (no admin needed) so that:
#
#   * `cmake.exe` and `clang.exe` resolve in any new shell
#     without manual prepending.
#   * `IOT_WHISPER_MODEL_PATH` is set by default (the smoke
#     tests + the daemon's `--stt-model` flag pick it up).
#
# After running this, close + reopen every shell so the new
# env propagates. Then `cargo test --features stt-whisper`
# / `cargo run -p iot-voice-daemon --features mic,stt-whisper`
# work from any directory.
#
# Use:
#
#   .\tools\setup-whisper-env.ps1
#   .\tools\setup-whisper-env.ps1 -Model X.bin   # custom model
#   .\tools\setup-whisper-env.ps1 -Revert        # undo (PATH only;
#                                                 # IOT_WHISPER_MODEL_PATH
#                                                 # cleared)
#
# Idempotent: if cmake/llvm are already on PATH, the script
# notices and doesn't double-add.

[CmdletBinding()]
param(
    [string]$Model = "$env:USERPROFILE\.iot-athome\models\ggml-base.en.bin",
    [switch]$Revert
)

$ErrorActionPreference = "Stop"

$cmakeBin = "C:\Program Files\CMake\bin"
$llvmBin = "C:\Program Files\LLVM\bin"

function Get-UserPath {
    [Environment]::GetEnvironmentVariable("PATH", "User")
}

function Set-UserPath {
    param([string]$Value)
    [Environment]::SetEnvironmentVariable("PATH", $Value, "User")
}

if ($Revert) {
    Write-Host "[setup-whisper-env] reverting User PATH + IOT_WHISPER_MODEL_PATH" -ForegroundColor Cyan
    $current = Get-UserPath
    $entries = $current -split ';' | Where-Object {
        $_ -ne $cmakeBin -and $_ -ne $llvmBin
    }
    Set-UserPath ($entries -join ';')
    [Environment]::SetEnvironmentVariable("IOT_WHISPER_MODEL_PATH", $null, "User")
    Write-Host "[setup-whisper-env] done (close + reopen shells to see effect)" -ForegroundColor Green
    exit 0
}

# Verify the directories exist before we add them — a typo'd
# PATH entry isn't fatal but it's clutter.
foreach ($dir in @($cmakeBin, $llvmBin)) {
    if (-not (Test-Path "$dir\")) {
        Write-Host "[setup-whisper-env] missing: $dir" -ForegroundColor Red
        Write-Host "  install with (elevated PowerShell):" -ForegroundColor Yellow
        Write-Host "    choco install cmake llvm" -ForegroundColor Yellow
        exit 1
    }
}

# Persist PATH additions, idempotently.
$current = Get-UserPath
$entries = $current -split ';'
$changed = $false

foreach ($dir in @($cmakeBin, $llvmBin)) {
    if ($entries -notcontains $dir) {
        Write-Host "[setup-whisper-env] adding $dir to User PATH" -ForegroundColor Cyan
        $entries += $dir
        $changed = $true
    } else {
        Write-Host "[setup-whisper-env] already on User PATH: $dir" -ForegroundColor DarkGray
    }
}

if ($changed) {
    Set-UserPath (($entries | Where-Object { $_ }) -join ';')
}

# Persist IOT_WHISPER_MODEL_PATH. Don't refuse to set it when
# the file's missing — the operator may want to download the
# model in a follow-up step. We just warn.
[Environment]::SetEnvironmentVariable("IOT_WHISPER_MODEL_PATH", $Model, "User")
Write-Host "[setup-whisper-env] IOT_WHISPER_MODEL_PATH = $Model" -ForegroundColor Cyan

if (-not (Test-Path $Model)) {
    Write-Host "" -ForegroundColor Yellow
    Write-Host "[setup-whisper-env] WARNING: model file does not exist yet at $Model" -ForegroundColor Yellow
    Write-Host "  Download it once with:" -ForegroundColor Yellow
    Write-Host "    `$dir = `"`$env:USERPROFILE\.iot-athome\models`"" -ForegroundColor Yellow
    Write-Host "    New-Item -ItemType Directory -Force `$dir | Out-Null" -ForegroundColor Yellow
    Write-Host "    Invoke-WebRequest -OutFile `"`$dir\ggml-base.en.bin`" ``" -ForegroundColor Yellow
    Write-Host "      -Uri `"https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin`"" -ForegroundColor Yellow
}

Write-Host "" -ForegroundColor Green
Write-Host "[setup-whisper-env] done." -ForegroundColor Green
Write-Host "  Close + reopen your shell so the new PATH/env propagates." -ForegroundColor Green
Write-Host "  Then:  .\tools\whisper-smoke.ps1" -ForegroundColor Green
