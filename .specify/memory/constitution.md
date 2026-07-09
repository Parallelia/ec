# Parallelia EC — Project Constitution

**Version**: 1.0.0  
**Ratification Date**: 2026-03-10  
**Last Amended**: 2026-03-10  
**Author**: Francisco Calderón (negrunch)

---

## Purpose

The Electoral Commission (`ec`) is the server-side daemon of the Parallelia voting system — an experimental, trustless, open-source electronic voting platform. Its mission is to enable **free, anonymous, verifiable, and censorship-resistant elections** for communities of any size, without requiring trust in any central authority beyond the cryptographic guarantees of the protocol itself.

---

## Principles

### Principle 1 — Voter Anonymity is Non-Negotiable

The EC **must never be able to link a cast vote to a specific voter identity**. This is enforced cryptographically via blind RSA signatures: the EC signs a token without seeing its content, so it cannot associate the token with the vote later. No logging, no side-channel, no shortcut may compromise this property.

**Rationale**: The entire value of the system collapses if the EC can identify who voted for whom. Anonymity is the threat model, not a nice-to-have.

### Principle 2 — Open Source and Auditable

All code must be MIT-licensed and publicly auditable. Every election event (announcements, tally updates) must be published to Nostr relays so any third party can independently verify results without trusting the EC operator.

**Rationale**: Consistent with Francisco's philosophy — "if you can't see the code, you can't open the hood."

### Principle 3 — Cryptography Over Trust

The system must rely on mathematical guarantees, not operator promises. The blind signature scheme must be the primary anti-fraud mechanism. No feature may be implemented that bypasses or weakens the cryptographic protocol for convenience.

**Rationale**: The target users are communities with low trust in central authorities. The system must work even if the EC operator is adversarial (within the limits of the single-EC model).

### Principle 4 — Decentralized Transport via Nostr

All voter-EC communication travels over Nostr using NIP-59 Gift Wrap encryption. Public election data (announcements, tallies) is published as addressable Nostr events. The system must not depend on any centralized messaging infrastructure.

**Rationale**: Censorship resistance. An election should not be stoppable by taking down a single server or blocking a single API.

### Principle 5 — Explicit Experimental Status

The system is experimental and not production-ready. The README and all public-facing documentation must clearly state this. No feature may be presented as security-audited unless a formal audit has been completed.

**Rationale**: Honesty with users. The author is not a cryptographer and the protocol has not been formally reviewed.

### Principle 6 — Minimal Dependencies, Proven Libraries

Dependencies must be chosen deliberately. For cryptographic operations, use only libraries that have been tested in production in this project or have wide community adoption. Do not introduce new crypto primitives without documented justification.

The current blind signature library is `blind-rsa-signatures = "0.17.1"`. Version 0.15.2 (used in the MVP) does not compile on Rust 1.86+ due to a `Eq/PartialEq` derive ambiguity bug in its transitive dependency `rsa 0.8`. Version 0.17.1 resolves this, uses the same blind RSA protocol, and compiles cleanly. The main API change: nonces are `[u8; 32]` instead of `BigUint`, and `num-bigint-dig` is no longer needed.

**Rationale**: Stability over novelty, but not at the cost of a broken build. The cryptographic protocol is identical.

### Principle 7 — Complete Voter Registration Flow

The voter registration process must be fully self-sovereign: the EC generates registration tokens that are distributed to eligible voters through an out-of-band channel, and voters register themselves using those tokens. The EC admin must never manually insert voter pubkeys — that would mean the EC knows the link between a real identity and a Nostr pubkey.

**Rationale**: This was the main unfinished piece of the MVP (issues #2–#5). Without this, anonymity is weakened because the admin knows who registered.

---

## Tech Stack (Non-Negotiable)

| Concern | Choice | Rationale |
|---|---|---|
| Language | Rust 1.94.0 (edition 2024) | Latest stable, type safety, performance |
| Blind signatures | `blind-rsa-signatures = "0.17.1"` | 0.15.2 broken on Rust 1.86+ (rsa 0.8 derive bug); 0.17.1 compiles clean, same crypto |
| Nostr transport | `nostr-sdk = "0.44.1"` with NIP-59 | Latest stable, censorship-resistant |
| Admin API | gRPC via `tonic = "0.14.5"` + `prost = "0.14.3"` | Clean interface for admin tooling |
| Persistence | SQLite via `sqlx = "0.8.6"` | Lightweight, embedded, no infra dependencies |
| Async runtime | `tokio = "1.50"` (multi-thread) | Latest stable |
| Serialization | `serde 1.0` + `serde_json 1.0` | Standard |
| Logging | `tracing 0.1` + `tracing-subscriber 0.3` | Structured |
| Hashing | `sha2 = "0.11.0-rc.5"` | Required by blind-rsa 0.17.1 (digest ^0.11) |
| Random | `rand = "0.10"` | Required by blind-rsa 0.17.1 |

---

## Architecture Boundaries

- **`ec`** is this repo — the Electoral Commission daemon only.
- **`voter`** is a separate repo — the voter client CLI/TUI.
- The two communicate exclusively via Nostr (NIP-59 Gift Wrap). No shared in-process state.
- The gRPC admin API is for EC operators only (localhost by default). It is never exposed to voters.

---

## Nostr Event Kinds

| Kind | Purpose |
|---|---|
| 35000 | Election announcement (addressable, replaceable) |
| 35001 | Vote tally update (addressable, replaceable) |
| 1059 | Gift Wrap — all encrypted voter↔EC messages |

---

## Governance

- **Amendments** require a PR with a clear rationale. The version must be bumped (MAJOR for breaking principle changes, MINOR for additions, PATCH for clarifications).
- **Compliance review**: every PR that touches cryptographic flows must reference the relevant principle(s) in the description.
- **Experimental clause**: until a formal security audit is completed, the constitution may evolve rapidly. All changes are tracked via git history.
