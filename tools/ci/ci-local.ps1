# IoT-AtHome — local CI runner (PowerShell, Windows-native).
#
# Use this instead of `just ci-local` on Windows when bash isn't a
# clean fit (e.g. your bash is WSL bash and Win32 interop adds
# friction, or Git Bash isn't installed). Same coverage as
# `just ci-local`: typos + fmt-check + clippy + workspace test +
# cargo-deny.
#
# Run:
#
#   .\tools\ci\ci-local.ps1
#
# Or from the repo root:
#
#   pwsh tools/ci/ci-local.ps1
#
# Exits non-zero on first failure (set -e equivalent via $ErrorActionPreference).
# Each step prints a banner so you can see which check ran when output
# scrolls.

[CmdletBinding()]
param(
    # Skip slow steps for fast iteration. Pass -Quick when you just
    # want fmt + typos (matches `just lint-fast`).
    [switch]$Quick
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $repoRoot

# Require cargo + the three cargo-installed CLIs to be on PATH.
# The error message tells the operator exactly which install command
# to run, so first-time setup is one read away.
function Test-Tool {
    param(
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [string]$InstallCmd
    )
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Write-Host "[ci-local] missing tool: $Name" -ForegroundColor Red
        Write-Host "  install:  $InstallCmd" -ForegroundColor Yellow
        exit 127
    }
}

Test-Tool -Name "cargo"  -InstallCmd "install Rust via https://rustup.rs"
Test-Tool -Name "typos"  -InstallCmd "cargo install typos-cli"

if (-not $Quick) {
    Test-Tool -Name "cargo-deny"     -InstallCmd "cargo install cargo-deny"
    Test-Tool -Name "cargo-nextest"  -InstallCmd "cargo install cargo-nextest"
}

function Run-Step {
    param(
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [scriptblock]$Block
    )
    $banner = "=== $Name ==="
    Write-Host $banner -ForegroundColor Cyan
    & $Block
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[ci-local] $Name failed (exit $LASTEXITCODE)" -ForegroundColor Red
        exit $LASTEXITCODE
    }
    Write-Host "$Name ok" -ForegroundColor Green
    Write-Host ""
}

# ---------------------------------------------------------- Fast layer

Run-Step "typos"      { typos . }
Run-Step "cargo fmt"  { cargo fmt --all -- --check }

if ($Quick) {
    Write-Host "[ci-local] -Quick mode: skipped clippy / test / audit" -ForegroundColor Yellow
    Write-Host "Run without -Quick before pushing source changes." -ForegroundColor Yellow
    exit 0
}

# ---------------------------------------------------------- Full layer

Run-Step "cargo clippy" {
    cargo clippy --workspace --all-targets -- -D warnings
}

Run-Step "cargo build" {
    cargo build --workspace --all-targets
}

Run-Step "cargo nextest" {
    cargo nextest run --workspace --all-targets
}

Run-Step "cargo deny" {
    cargo deny check
}

Write-Host "[ci-local] all checks passed" -ForegroundColor Green
