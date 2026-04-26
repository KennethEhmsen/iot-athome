# IoT-AtHome — canonical task runner
# Run `just` to list all tasks.
#
# Conventions:
#   - One verb per user action. No silent multi-step macros without a clear name.
#   - Tasks that don't exist yet are listed as TODO so contributors see the shape.

set shell := ["bash", "-cu"]
set dotenv-load := false

# --- meta ---

default:
    @just --list --unsorted

# Run every check CI runs, locally, in the right order. Use this before
# `git push` on **source changes** to avoid round-tripping through CI.
ci-local: ci-preflight ci-build ci-test ci-audit

# Fast pre-push subset for **docs-only** changes (retros, READMEs, ADRs,
# this Justfile). Just typos + fmt-check — ~5s vs. the multi-minute
# `ci-local` run. Catches the M5a-tag-prep class of avoidable round
# trips (`Activ` and `overrideable` slipping into committed prose;
# typos triggers preflight in CI which then takes the whole pipeline
# down on a one-character fix).
lint-fast: lint-typos lint-fmt

ci-preflight: lint-typos lint-fmt lint-schemas lint-rust lint-panel

ci-build:
    cargo build --workspace --all-targets

ci-test:
    cargo nextest run --workspace --all-targets

ci-audit:
    cargo deny check

# Prereq: `cargo install typos-cli` (CI uses the crate-ci/typos GitHub
# action which fetches its own binary; the local check needs the
# cargo-installed CLI). Allow-list lives in `_typos.toml` at repo root.
lint-typos:
    typos .

lint-fmt:
    cargo fmt --all -- --check

# --- dev loop ---

# Spin up the full local infrastructure stack (NATS, Mosquitto, Keycloak, Envoy, Tempo/Loki/Prometheus).
dev:
    docker compose -f deploy/compose/dev-stack.yml up -d
    @echo "Dev stack up. 'just dev-logs' to tail logs, 'just dev-down' to stop."

# Tail logs from the dev stack.
dev-logs:
    docker compose -f deploy/compose/dev-stack.yml logs -f --tail=200

# Stop the dev stack (preserves volumes).
dev-down:
    docker compose -f deploy/compose/dev-stack.yml down

# Nuke the dev stack AND its volumes (certs, DB data, NATS streams).
dev-nuke:
    docker compose -f deploy/compose/dev-stack.yml down -v

# Mint dev CA + component certificates (run once before `just dev`).
certs:
    ./tools/devcerts/mint.sh

# Wipe and recreate dev CA + certs.
certs-reset:
    rm -rf tools/devcerts/generated
    ./tools/devcerts/mint.sh

# --- build ---

# Build everything (Rust workspace + schemas + panel + Python ml service).
build: schemas build-rust build-panel build-ml

build-rust:
    cargo build --workspace --all-targets

build-panel:
    @if [ -d panel ] && [ -f panel/package.json ]; then pnpm -C panel install --frozen-lockfile && pnpm -C panel build; else echo "(panel not scaffolded yet — skipping)"; fi

build-ml:
    @if [ -f services/ml/pyproject.toml ]; then uv --project services/ml sync; else echo "(ml service not scaffolded yet — skipping)"; fi

# Cross-compile Rust services for aarch64 (Raspberry Pi).
build-aarch64:
    cargo build --workspace --target aarch64-unknown-linux-gnu --release

# --- schemas ---

# Lint, breaking-change check, and codegen all schemas.
schemas: schemas-lint schemas-breaking schemas-gen

schemas-lint:
    buf lint schemas

schemas-breaking:
    @git rev-parse --verify main >/dev/null 2>&1 && buf breaking schemas --against '.git#branch=main' || echo "(no main branch yet — skipping breaking-change check)"

schemas-gen:
    @echo "TODO: wire buf.gen.yaml generation once proto targets exist in crates/iot-proto"

# --- test ---

test: test-rust test-panel test-ml

test-rust:
    cargo nextest run --workspace --all-targets

test-panel:
    @if [ -d panel ] && [ -f panel/package.json ]; then pnpm -C panel test; else echo "(panel not scaffolded yet)"; fi

test-ml:
    @if [ -f services/ml/pyproject.toml ]; then uv --project services/ml run pytest; else echo "(ml service not scaffolded yet)"; fi

# --- lint / format ---

lint: lint-rust lint-schemas lint-panel lint-ml

lint-rust:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

lint-schemas:
    buf lint schemas

lint-panel:
    @if [ -d panel ] && [ -f panel/package.json ]; then pnpm -C panel lint; else echo "(panel not scaffolded yet)"; fi

lint-ml:
    @if [ -f services/ml/pyproject.toml ]; then uv --project services/ml run ruff check; else echo "(ml service not scaffolded yet)"; fi

fmt:
    cargo fmt --all
    @if [ -d panel ]; then pnpm -C panel prettier --write .; fi
    @if [ -f services/ml/pyproject.toml ]; then uv --project services/ml run ruff format; fi

# --- security / supply chain ---

audit:
    cargo audit
    @if [ -f services/ml/pyproject.toml ]; then uv --project services/ml run pip-audit; fi

sbom:
    @mkdir -p sbom
    cargo cyclonedx --all --format json -o sbom/
    @echo "SBOM(s) in ./sbom/. Sign with 'just sign-sbom'."

sign-sbom:
    @echo "TODO: cosign sign-blob over every file in ./sbom/ (CI does this keyless)."

# --- release ---

release VERSION:
    @echo "TODO: release ceremony for {{VERSION}} (see ADR-0006)."

# --- hygiene ---

clean:
    cargo clean
    rm -rf sbom/ target/

# --- docs ---

# Rebuild the design Word document from docs/build-doc/.
doc-build:
    cd docs/build-doc && node build-doc.js && mv IoT-AtHome-Design.docx ../IoT-AtHome-Design.docx

# Show project stats (LoC, ADR count, crate count).
stats:
    @echo "ADRs:         $(ls docs/adr/*.md 2>/dev/null | wc -l)"
    @echo "Crates:       $(ls -d crates/*/ 2>/dev/null | wc -l)"
    @echo "Plugins:      $(ls -d plugins/*/ 2>/dev/null | wc -l)"
    @echo "Proto files:  $(find schemas -name '*.proto' 2>/dev/null | wc -l)"
