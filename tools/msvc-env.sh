#!/usr/bin/env bash
# Source this file from git-bash / WSL to set PATH + MSVC env vars so
# `cargo build` can find link.exe + the Windows SDK on this Windows host.
#
# Usage:
#   source tools/msvc-env.sh
#   cargo build --workspace
#
# Detects the MSVC install under C:\Program Files (x86)\Microsoft Visual Studio
# and the latest Windows SDK under C:\Program Files (x86)\Windows Kits\10.

set -u

# --- Visual Studio Build Tools ---
vs_root="/c/Program Files (x86)/Microsoft Visual Studio/18/BuildTools"
if [[ ! -d "$vs_root" ]]; then
  echo "msvc-env: $vs_root not found" >&2
  return 1 2>/dev/null || exit 1
fi

msvc_ver=$(ls "$vs_root/VC/Tools/MSVC/" | sort -V | tail -1)
msvc_root="$vs_root/VC/Tools/MSVC/$msvc_ver"

# --- Windows 10/11 SDK ---
sdk_root="/c/Program Files (x86)/Windows Kits/10"
sdk_ver=$(ls "$sdk_root/Include/" | sort -V | tail -1)

# --- Cargo / Rust tools we care about on PATH ---
cargo_bin="${HOME:-/c/Users/${USER:-${USERNAME:-kenne}}}/.cargo/bin"

# --- Compose PATH ---
PATH="$cargo_bin:\
$msvc_root/bin/Hostx64/x64:\
$sdk_root/bin/$sdk_ver/x64:\
$PATH"
export PATH

# --- INCLUDE / LIB env vars rustc/cc-rs consult ---
export INCLUDE="\
$msvc_root/include;\
$sdk_root/Include/$sdk_ver/ucrt;\
$sdk_root/Include/$sdk_ver/shared;\
$sdk_root/Include/$sdk_ver/um;\
$sdk_root/Include/$sdk_ver/winrt"

export LIB="\
$msvc_root/lib/x64;\
$sdk_root/Lib/$sdk_ver/ucrt/x64;\
$sdk_root/Lib/$sdk_ver/um/x64"

export LIBPATH="$msvc_root/lib/x64"

echo "msvc-env: MSVC $msvc_ver + Windows SDK $sdk_ver ready"
command -v link.exe >/dev/null && echo "  link.exe: $(command -v link.exe)"
command -v cl.exe >/dev/null && echo "  cl.exe  : $(command -v cl.exe)"
command -v cargo >/dev/null && echo "  cargo   : $(command -v cargo)"
