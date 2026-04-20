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
# Requirements: openssl >= 1.1.1 (or `step-cli`). This script uses openssl
# because it's universally available; a step-cli variant is welcome later.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${HERE}/generated"
CA_DIR="${OUT}/ca"
DAYS=825         # ~2.25 years — short enough that dev envs regenerate regularly.

mkdir -p "${OUT}" "${CA_DIR}"

log() { printf '\e[1;36m[devcerts]\e[0m %s\n' "$*"; }

# ---------- Root CA ----------

if [[ ! -f "${CA_DIR}/ca.crt" ]]; then
  log "Creating local dev CA"
  openssl genrsa -out "${CA_DIR}/ca.key" 4096 2>/dev/null
  openssl req -x509 -new -nodes \
    -key "${CA_DIR}/ca.key" \
    -sha256 -days 1825 \
    -subj "/C=XX/O=IoT-AtHome-Dev/CN=IoT-AtHome Dev Root CA" \
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

  cat > "${cnf}" <<EOF
[req]
distinguished_name = req_dn
req_extensions     = v3_req
prompt             = no

[req_dn]
C  = XX
O  = IoT-AtHome-Dev
CN = ${cn}

[v3_req]
keyUsage         = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth, clientAuth
subjectAltName   = @alt_names

[alt_names]
EOF

  local i=1
  for san in "${sans[@]}"; do
    if [[ "${san}" == IP:* ]]; then
      echo "IP.${i} = ${san#IP:}" >> "${cnf}"
    else
      echo "DNS.${i} = ${san}" >> "${cnf}"
    fi
    i=$((i+1))
  done

  openssl genrsa -out "${key}" 2048 2>/dev/null
  openssl req -new -key "${key}" -out "${csr}" -config "${cnf}"
  openssl x509 -req -in "${csr}" \
    -CA "${CA_DIR}/ca.crt" -CAkey "${CA_DIR}/ca.key" -CAcreateserial \
    -out "${crt}" -days "${DAYS}" -sha256 \
    -extensions v3_req -extfile "${cnf}" 2>/dev/null
  chmod 600 "${key}"
}

# ---------- Components ----------
#
# Keep this list aligned with deploy/compose/dev-stack.yml service hostnames.

log "Minting component certs"

mint_component nats      "nats.iot.local"      DNS:nats.iot.local      DNS:localhost IP:127.0.0.1
mint_component mosquitto "mosquitto.iot.local" DNS:mosquitto.iot.local DNS:localhost IP:127.0.0.1
mint_component gateway   "gateway.iot.local"   DNS:gateway.iot.local   DNS:localhost IP:127.0.0.1
mint_component registry  "registry.iot.local"  DNS:registry.iot.local  DNS:localhost IP:127.0.0.1
mint_component envoy     "envoy.iot.local"     DNS:envoy.iot.local     DNS:localhost IP:127.0.0.1
mint_component panel     "panel.iot.local"     DNS:panel.iot.local     DNS:localhost IP:127.0.0.1

# Client cert used by `iotctl` and by the panel's device identity during dev.
mint_component client    "dev-client"          DNS:dev-client          DNS:localhost

log "Done. Certs in ${OUT}/"
log "Add '${OUT}/ca/ca.crt' to your OS trust store for a friction-free dev loop."
