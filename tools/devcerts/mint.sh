#!/usr/bin/env bash
# IoT-AtHome — dev certificate mint.
#
# Generates a local CA and component certificates for mTLS between
# every dev-time component (NATS, Mosquitto, Envoy/Gateway, Registry, CLI).
#
# WARNING: These certs are for LOCAL DEVELOPMENT ONLY. They have no relation
# to the production signing hierarchy described in ADR-0006. Do not ship
# them. Do not copy them anywhere. The `.gitignore` excludes generated output.
#
# Requirements: openssl >= 1.1.1. This script uses openssl because it's
# universally available; a step-cli variant is welcome later.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${HERE}/generated"
CA_DIR="${OUT}/ca"
DAYS=825         # ~2.25 years — short enough that dev envs regenerate regularly.

mkdir -p "${OUT}" "${CA_DIR}"

log() { printf '\e[1;36m[devcerts]\e[0m %s\n' "$*"; }

# ---------- Root CA ----------

ca_cnf="${CA_DIR}/ca.cnf"
cat > "${ca_cnf}" <<'EOF'
[req]
distinguished_name = req_dn
x509_extensions    = v3_ca
prompt             = no

[req_dn]
C  = XX
O  = IoT-AtHome-Dev
CN = IoT-AtHome Dev Root CA

[v3_ca]
basicConstraints = critical, CA:true
keyUsage         = critical, digitalSignature, cRLSign, keyCertSign
subjectKeyIdentifier = hash
EOF

if [[ ! -f "${CA_DIR}/ca.crt" ]]; then
  log "Creating local dev CA"
  openssl genrsa -out "${CA_DIR}/ca.key" 4096
  openssl req -x509 -new -nodes \
    -key "${CA_DIR}/ca.key" \
    -sha256 -days 1825 \
    -config "${ca_cnf}" \
    -out "${CA_DIR}/ca.crt"
  chmod 600 "${CA_DIR}/ca.key"
else
  log "Dev CA already exists at ${CA_DIR}/ca.crt"
fi

# ---------- Component cert minter ----------

mint_component() {
  local name="$1"
  local cn="$2"
  shift 2
  local sans=("$@")

  local dir="${OUT}/${name}"
  mkdir -p "${dir}"

  local key="${dir}/${name}.key"
  local csr="${dir}/${name}.csr"
  local crt="${dir}/${name}.crt"
  local cnf="${dir}/${name}.cnf"

  if [[ -f "${crt}" ]]; then
    log "  ${name}: already exists"
    return
  fi

  log "  ${name}: generating"

  {
    echo "[req]"
    echo "distinguished_name = req_dn"
    echo "req_extensions     = v3_req"
    echo "prompt             = no"
    echo
    echo "[req_dn]"
    echo "C  = XX"
    echo "O  = IoT-AtHome-Dev"
    echo "CN = ${cn}"
    echo
    echo "[v3_req]"
    echo "keyUsage         = critical, digitalSignature, keyEncipherment"
    echo "extendedKeyUsage = serverAuth, clientAuth"
    echo "subjectAltName   = @alt_names"
    echo
    echo "[alt_names]"
    local i=1
    for san in "${sans[@]}"; do
      if [[ "${san}" == IP:* ]]; then
        echo "IP.${i} = ${san#IP:}"
      else
        echo "DNS.${i} = ${san#DNS:}"
      fi
      i=$((i+1))
    done
  } > "${cnf}"

  openssl genrsa -out "${key}" 2048
  openssl req -new -key "${key}" -out "${csr}" -config "${cnf}"
  openssl x509 -req -in "${csr}" \
    -CA "${CA_DIR}/ca.crt" -CAkey "${CA_DIR}/ca.key" -CAcreateserial \
    -out "${crt}" -days "${DAYS}" -sha256 \
    -extensions v3_req -extfile "${cnf}"
  chmod 600 "${key}"
}

# ---------- Components ----------
#
# Keep this list aligned with deploy/compose/dev-stack.yml service hostnames.

log "Minting component certs"

mint_component nats           "nats.iot.local"      DNS:nats.iot.local      DNS:localhost IP:127.0.0.1
mint_component mosquitto      "mosquitto.iot.local" DNS:mosquitto.iot.local DNS:localhost IP:127.0.0.1
mint_component gateway        "gateway.iot.local"   DNS:gateway.iot.local   DNS:localhost IP:127.0.0.1
mint_component registry       "registry.iot.local"  DNS:registry.iot.local  DNS:localhost IP:127.0.0.1
mint_component envoy          "envoy.iot.local"     DNS:envoy.iot.local     DNS:localhost IP:127.0.0.1
mint_component panel          "panel.iot.local"     DNS:panel.iot.local     DNS:localhost IP:127.0.0.1

# Adapter identities — one per plugin. Mosquitto's mTLS listener reads the
# CN as the MQTT username (via use_identity_as_username true), so each
# adapter has its own cert.
mint_component zigbee-adapter "zigbee-adapter"      DNS:zigbee-adapter      DNS:localhost

# Client cert used by `iotctl` and by the panel's device identity during dev.
mint_component client         "dev-client"          DNS:dev-client          DNS:localhost

# ---------- NATS decentralized-auth trust root ----------
#
# Mints the operator + account keypair pair the broker reads via its
# `operator:` directive + memory-resolver `resolver_preload` map. Per-
# plugin user JWTs are minted later, by `iotctl plugin install`, against
# the account seed produced here.
#
# Idempotent: skips if the operator / account files exist; pass
# `--force-nats` to regenerate (DESTRUCTIVE — invalidates every user
# JWT minted under the previous account key).

NATS_TRUST_DIR="${OUT}/nats"
mkdir -p "${NATS_TRUST_DIR}"

if [[ ! -f "${NATS_TRUST_DIR}/iot-account.jwt" || "${1:-}" == "--force-nats" ]]; then
  log "Bootstrapping NATS operator + account trust root"

  # Build iotctl on demand if it isn't already on PATH. The release
  # build stays cached across invocations so this is a one-time cost.
  IOTCTL="${IOTCTL:-iotctl}"
  if ! command -v "${IOTCTL}" >/dev/null 2>&1; then
    log "  iotctl not on PATH — falling back to 'cargo run -p iot-cli --bin iotctl --quiet'"
    IOTCTL="cargo run -p iot-cli --bin iotctl --quiet --"
  fi

  FORCE_FLAG=""
  if [[ "${1:-}" == "--force-nats" ]]; then
    FORCE_FLAG="--force"
  fi

  ${IOTCTL} nats bootstrap --out "${NATS_TRUST_DIR}" ${FORCE_FLAG} \
    --account-name IOT
else
  log "  NATS trust root already exists at ${NATS_TRUST_DIR}/iot-account.jwt"
fi

log "Done. Certs in ${OUT}/"
log "Add '${OUT}/ca/ca.crt' to your OS trust store for a friction-free dev loop."
log "NATS account seed: ${NATS_TRUST_DIR}/iot-account.nk"
log "  → export IOT_NATS_ACCOUNT_SEED=${NATS_TRUST_DIR}/iot-account.nk"
log "    before 'iotctl plugin install' to mint per-plugin user JWTs."
