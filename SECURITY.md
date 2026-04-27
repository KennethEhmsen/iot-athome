# Security policy

Thank you for taking the time to look. Reports of security
vulnerabilities are taken seriously and triaged within
**5 business days** of receipt.

## Reporting a vulnerability

**Email:** `security@iot-athome.example` *(M6 W1 deliverable —
real address replaces this placeholder before `v1.0.0`)*

**PGP key:** Pinned at `https://iot-athome.example/.well-known/security.txt`
once published. Key fingerprint will be cross-published on
keys.openpgp.org. Use it for any sensitive details.

**GitHub Security Advisories:** Alternatively, file a private
[security advisory](https://github.com/KennethEhmsen/iot-athome/security/advisories/new)
on this repository. The maintainer is notified directly and the
advisory stays embargoed until disclosure.

## Disclosure timeline

* **5 business days** — initial acknowledgement.
* **30 days** — initial triage + severity classification + a
  patch ETA.
* **90 days** — coordinated disclosure window. We aim to ship a
  fix and credit the reporter on the same date. Extensions
  granted in writing for issues whose patches require structural
  rework or third-party coordination.

A reporter who waits longer than 90 days for a fix is welcome to
disclose publicly; we won't dispute the timeline.

## Scope

In scope:

* The `iot-gateway` HTTP / WebSocket surface (REST `/api/v1/*` +
  the WebSocket `/stream` channel).
* The panel PWA's auth flow + content-security boundary.
* The plugin-install signature path (`iotctl plugin install` —
  cosign blob signature, SBOM CVE check, capability ACL).
* The bus subject authorisation model (NATS account / user JWT
  chain, MQTT broker ACL).
* Any first-party WASM plugin shipped from this repository.

Out of scope:

* Vulnerabilities in third-party dependencies that we transitively
  consume but don't expose new attack surface for. (Report those
  upstream; we'll bump our pin after the upstream patch.)
* Issues that require physical access to the hub hardware.
* Reports about missing `Strict-Transport-Security` or similar
  hardening headers on the *dev-mode* loopback gateway. The prod
  Envoy front-end (M3) is the surface to test.

## Recognition

Researchers who file responsible-disclosure reports — embargo
respected, sufficient detail to reproduce, no extortion — are
credited in the corresponding release notes' security section,
unless they request anonymity. We don't run a bug-bounty program
at this time; recognition is non-monetary.

## Anti-vendor-shopping note

This is a small, single-maintainer project. We don't have a
24/7 SOC; reports filed late on a Friday land in a backlog until
Monday. We try to do better than that for confirmed RCEs.

— *Last updated: M6 W0 (this commit; pre-`v1.0.0`).*
