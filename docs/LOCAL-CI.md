# Local CI

You can run essentially everything the GitHub `ci` workflow does on
your own machine for free. The only jobs you can't fully replicate
locally are the **tag-only** release-ceremony pieces (cosign keyless
needs the GitHub OIDC token; SLSA provenance needs the GH attestation
API). Everything else — preflight lints, builds, unit tests,
integration tests with testcontainers, supply-chain audits — runs
identically to CI.

## Why care

GitHub Actions costs add up. Burning runner minutes on a fmt-drift
fix is wasteful when `cargo fmt --check` runs in 2 seconds locally.
Treat CI as the **final** gate before tagging, not the iteration
loop.

## Prereqs (one-time)

```sh
cargo install typos-cli       # for `just lint-typos`
cargo install cargo-nextest   # for `just ci-test`
cargo install cargo-deny      # for `just ci-audit`
cargo install cargo-cyclonedx # for SBOM generation (optional, only at tag time)
```

Plus Docker / Podman / OrbStack for the integration tests
(testcontainers spins NATS, Mosquitto, Postgres + TimescaleDB
images).

## Windows: skip `just`, use the PowerShell helper

`just` runs each recipe through `bash -cu` (per `set shell` in the
Justfile). On Windows, that bash is either Git for Windows bash
(`/c/...` paths) or WSL bash (`/mnt/c/...` paths) — and **neither
reliably runs the Windows-side cargo binaries** the way PowerShell
does. Git Bash needs `%USERPROFILE%\.cargo\bin` on its PATH (which
isn't inherited from PowerShell unless you've made it persistent);
WSL bash needs Win32 interop + the `.exe` suffix to find typos /
cargo-nextest / cargo-deny.

Sidestep the whole thing: use the PowerShell helper that does the
same work without bash:

```powershell
.\tools\ci\ci-local.ps1            # full CI parity (typos + fmt + clippy + build + test + deny)
.\tools\ci\ci-local.ps1 -Quick     # docs-only fast subset (typos + fmt-check, ~5 s)
```

The helper:

* Verifies your prereqs (cargo, typos, cargo-deny, cargo-nextest)
  and prints the exact `cargo install` command if anything's
  missing.
* Runs each step with a banner so you know what failed if anything
  does.
* Bails on first failure (no point running `cargo deny` if the
  build is broken).

Use `-Quick` for the equivalent of `just lint-fast` — typos +
fmt-check only, ~5 s. Use no flag for the equivalent of
`just ci-local` — full coverage, a few minutes.

If you'd rather use `just ci-local` directly: add cargo bin to the
**persistent user PATH** so Git Bash sees it. From PowerShell:

```powershell
[Environment]::SetEnvironmentVariable(
  "PATH",
  "$env:PATH;$env:USERPROFILE\.cargo\bin",
  "User"
)
```

Close + reopen your shell. This works for Git Bash; WSL bash needs
extra `.exe`-resolution config that's not worth the trouble — just
use the PowerShell helper there.

## The day-to-day workflow

```sh
# 1. Make changes.

# 2. Before pushing — full local CI:
just ci-local

# Or for docs-only changes (~5s):
just lint-fast
```

`just ci-local` expands to:

| Step | Command | What it covers |
|---|---|---|
| `ci-preflight` | `lint-typos` + `lint-fmt` + `lint-schemas` + `lint-rust` + `lint-panel` | Style + spelling + workspace-wide clippy `-D warnings` |
| `ci-build` | `cargo build --workspace --all-targets` | All bins + libs + tests compile |
| `ci-test` | `cargo nextest run --workspace --all-targets` | Every workspace unit test |
| `ci-audit` | `cargo deny check` | Advisories + bans + licenses + sources |

That's a 1:1 match for the GitHub workflow's `preflight`, `build`,
`test`, and `vuln` jobs.

## Windows: seed-file ACLs

`iotctl nats bootstrap` and `iotctl plugin install` write nkey
seeds + NATS creds files to the install dir. On Unix those are
`chmod 0600`'d after write; on Windows the same code path shells
out to `icacls` to strip inherited ACEs and grant the current
user full control only — no other principal can read the seed.

The Unix path is exercised by every `cargo test -p iot-cli` run
on the main `test` job; the Windows path runs on the `test-windows`
job in `.github/workflows/ci.yml`. If you're hardening that path
locally on Windows, run `cargo test -p iot-cli --bins secrets::` —
both `windows_grants_owner_only` and `idempotent_on_same_file`
exercise it.

The TLS `*.key` files `tools/devcerts/mint.sh` writes are NOT
covered by this path (mint.sh's `chmod 600` is a no-op on
NTFS). Treat the dev TLS material as workstation-local; don't
reuse it off-machine. See ADR-0011's Windows hardening note for
the broader story.

## Integration tests (Docker required)

The CI's `integration` job runs:

```sh
bash ./tools/devcerts/mint.sh
docker compose -f deploy/compose/dev-stack.yml up -d nats mosquitto
cargo test --workspace --test '*' -- --test-threads=1
```

Locally, do the same:

```sh
just certs                 # mint dev CA + component certs (idempotent)
just dev                   # bring up the compose stack
cargo test --workspace --test '*' -- --test-threads=1
just dev-down              # stop the compose stack (keeps volumes)
```

The `iot-history` integration tests in
`crates/iot-history/tests/postgres_roundtrip.rs` and
`plain_postgres_compat.rs` self-skip when no Docker runtime is
detected, so they pass cleanly even without the compose stack
running. To exercise them live:

```sh
docker compose -f deploy/compose/dev-stack.yml --profile history up -d timescale
cargo test -p iot-history --test postgres_roundtrip
cargo test -p iot-history --test plain_postgres_compat
```

## SBOM generation

```sh
cargo cyclonedx --all --format json
find . -name 'bom.json' -exec cp {} sbom/ \;
```

Same as the CI `sbom` job. Output ends up in `sbom/`.

## Release-ceremony jobs (tag-only — can't fully run locally)

Three jobs only fire on `git push --tags` for `v*` tags:

| Job | What it does | Local replacement |
|---|---|---|
| `reproducibility` | Two `cargo build --release` runs + `diffoscope` | Run the two builds locally + run `diffoscope` manually. Practical but slow (~15 min per build × 2). |
| `sign` | cosign keyless sign-blob + Rekor entry | **Needs GitHub OIDC**. Can't replicate without a GitHub-issued token. |
| `publish` | GH Releases artifact upload | **Needs the GH API**. Skip locally. |

For tag releases, push the tag and let GitHub Actions handle these
three. They run in a few minutes once the build artefacts are
uploaded.

## Full GHA emulation with `act` (optional)

If you want byte-exact CI parity locally (including matrix builds
+ integration with the same runner image GitHub uses),
[nektos/act](https://github.com/nektos/act) reads
`.github/workflows/ci.yml` and runs each job in a Docker container
that mimics the GitHub runner.

```sh
choco install act-cli         # Windows
# or scoop install act         # Windows (scoop)
# or brew install act           # macOS
# or download binary from https://github.com/nektos/act/releases

act -j preflight                  # run one job
act -j test
act -j integration                # needs Docker for testcontainers AND for act
act --list                        # show what's available
act push                          # simulate a push event (runs all jobs)
```

Caveats:

- First run downloads the runner image (multi-GB). Subsequent runs
  are fast.
- Some actions don't run identically. Cosign keyless and SLSA
  provenance need GitHub OIDC tokens that `act` can't issue, so
  those steps fail under `act` the same way they'd fail on CI
  without runner credentials.
- `act` honours the workflow's matrix strategies, so `build-x86_64`
  and `build-aarch64` both run when you run `act -j build`.

For everyday work, `just ci-local` is enough. `act` is the "I'm
debugging a CI-only failure" tool.

## Recommended cadence

* **Every commit:** `just lint-fast` (~5 s)
* **Before push:** `just ci-local` (a few minutes)
* **Before tag:** `just ci-local` + spin the compose stack +
  `cargo test --workspace --test '*'` to cover integration
* **At tag:** push to GitHub, let the runners do the
  reproducibility / sign / publish dance

## Tag-time CI billing

Even with the best local discipline, the tag-only jobs (sign +
publish + reproducibility) need to run on GitHub. If GitHub Actions
billing blocks them, the tag still exists and is signed via your
local cosign / git tag — but the **public** signature path (Rekor
+ cosign keyless OIDC) requires GitHub's OIDC token, which only
the GitHub runner can issue. There's no local workaround for
keyless cosign by design.

The mitigation is to run **everything except** the tag-only jobs
locally, so day-to-day work doesn't burn runner minutes — and only
push tags when GitHub billing is healthy.
