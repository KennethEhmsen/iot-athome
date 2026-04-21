#!/usr/bin/env bash
# Boot the full local dev stack + all iot-* services with correct bus certs.
#
# Usage:
#   bash tools/run-dev.sh     # starts services in background, writes var/*.log
#
# Services can then be stopped with `bash tools/stop-dev.sh`.

set -euo pipefail
cd "$(dirname "$0")/.."

CERTS="./tools/devcerts/generated"
mkdir -p var

common_bus_env() {
  local component="$1"
  echo "IOT_BUS__URL=tls://127.0.0.1:4222"
  echo "IOT_BUS__CA_PATH=$CERTS/ca/ca.crt"
  echo "IOT_BUS__CLIENT_CERT_PATH=$CERTS/$component/$component.crt"
  echo "IOT_BUS__CLIENT_KEY_PATH=$CERTS/$component/$component.key"
  echo "IOT_BUS__PUBLISHER=$component"
}

start() {
  local name="$1" bin="$2" component="$3"
  echo "starting $name"
  env $(common_bus_env "$component" | xargs) "$bin" > "var/$name.log" 2>&1 &
  echo "  pid=$!"
}

start iot-registry        "./target/release/iot-registry.exe"        registry
sleep 1
start iot-gateway         "./target/release/iot-gateway.exe"         gateway
start zigbee2mqtt-adapter "./target/release/zigbee2mqtt-adapter.exe" zigbee-adapter

echo
echo "logs: var/iot-registry.log, var/iot-gateway.log, var/zigbee2mqtt-adapter.log"
echo "gateway http: http://127.0.0.1:8081/healthz"
echo "stop:  bash tools/stop-dev.sh"
