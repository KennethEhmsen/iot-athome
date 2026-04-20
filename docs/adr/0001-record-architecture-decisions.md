# ADR-0001: Record Architecture Decisions

- **Status:** Accepted
- **Date:** 2026-04-20
- **Deciders:** IoT-AtHome core team

## Context

We are starting a long-lived systems project with a small team. Architecture decisions made in the first weeks will shape the codebase for years. Oral tradition and chat history are unreliable records. Decisions also need to be auditable: this platform targets ETSI EN 303 645 / ISO 27001 alignment, and "why was this done?" must be answerable from the repo.

## Decision

We use **Architecture Decision Records (ADRs)** in Michael Nygard's format, stored in `docs/adr/` as Markdown files. File naming: `NNNN-short-kebab-title.md`, monotonically numbered, never renumbered.

Every decision that meets **any one** of these tests becomes an ADR:

- Affects the public shape of an API, schema, or plugin ABI.
- Locks a dependency or runtime we would regret losing (async runtime, event bus, etc.).
- Defines a security-relevant invariant (signing, auth, isolation).
- Is expensive to reverse (> 1 week of work to undo).

**Status lifecycle:** `Proposed` → `Accepted` → (`Superseded by NNNN` | `Deprecated`). ADRs are **append-only**; changes produce a new ADR that supersedes the old. The old ADR is edited only to flip its status line and add a link.

**Format:**

```
# ADR-NNNN: Title

- Status:
- Date:
- Deciders:

## Context
## Decision
## Consequences
```

## Consequences

- Small but non-zero friction for every architecture change (~30 minutes to write).
- The repo becomes self-documenting for reviewers and future maintainers.
- PR review for ADR-triggering changes requires the ADR in the same PR; CI enforces this via a label check (`needs-adr`).
