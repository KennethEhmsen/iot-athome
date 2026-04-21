#!/usr/bin/env bash
# Stop the background services started by `run-dev.sh`.

set -euo pipefail
powershell.exe -NoProfile -Command \
  "Get-Process iot-registry,iot-gateway,zigbee2mqtt-adapter -ErrorAction SilentlyContinue | Stop-Process -Force" \
  2>/dev/null || true
echo "stopped iot-* services"
