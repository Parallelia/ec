# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Parallelia EC is the Electoral Commission daemon for a trustless electronic voting system. It manages elections, issues anonymous voting tokens via blind RSA signatures, communicates with voters over Nostr (NIP-59 Gift Wrap), and publishes verifiable results. **Experimental, unaudited software.**

## Build & Test Commands

```sh
cargo build                                          # build
cargo test                                           # run all tests
cargo test crypto_test                               # run a single test file
cargo test test_crypto_roundtrip                     # run a single test
cargo clippy --all-targets --all-features -- -D warnings  # lint (must pass clean)
cargo fmt                                            # format
cargo fmt -- --check                                 # check formatting without modifying
```

Tests live in `tests/` as integration tests (e.g. `tests/crypto_test.rs`, `tests/counting_plurality_test.rs`, `tests/counting_stv_test.rs`). They import from the `ec` library crate.

SQLite migrations run automatically at startup via `sqlx::migrate!("./migrations")`. For manual migration: `sqlx migrate run`.

## Architecture

Single Rust binary (`src/main.rs`) with three planned surfaces:

- **Nostr listener/publisher** — voter communication via NIP-59 Gift Wrap (nostr-sdk 0.44.1)
- **gRPC admin API** — operator interface (tonic 0.14.5, proto files not yet added)
- **Scheduler** — drives election state transitions and counting

### Module Layout (`src/`)

| Module | Purpose |
|---|---|
| `config.rs` | Hybrid config: `ec.toml` (non-secrets) + env vars (secrets). Precedence: env > toml > defaults |
| `crypto.rs` | Blind RSA signatures (blind-rsa-signatures 0.17.1). Keypair gen, blind sign, verify |
| `db.rs` | All SQLite queries. Registration token + authorized_voter writes use transactions with `rows_affected()` checks |
| `types.rs` | Domain structs: Election, Candidate, RegistrationToken, AuthorizedVoter, Vote, UsedNonce |
| `state.rs` | `AppState` (db pool, nostr client, keys, config) shared via `Arc` |
| `rules/` | Election rule loading from TOML files in `rules/` directory. `ElectionRules` struct |
| `counting/` | `CountingAlgorithm` trait + implementations. `algorithm_for()` registry dispatches by rules_id |

### Counting System

New counting methods: implement `CountingAlgorithm` trait → register in `algorithm_for()` in `counting/mod.rs` → add a `.toml` in `rules/`. Current implementations: `plurality`, `stv`.

### Config & Secrets

- Non-secrets: `ec.toml` (versioned). Env vars override: `RELAY_URL`, `GRPC_BIND`, `RULES_DIR`, `LOG_LEVEL`, `DATABASE_URL`
- Secrets: env vars only, wrapped in `SecretString`. Required: `NOSTR_PRIVATE_KEY`. Optional: `EC_DB_PASSWORD`
- Dev: `cp .env.example .env` — loaded by `dotenvy` at startup

## Critical Rules

1. **Voter anonymity is non-negotiable.** Never store a link between a vote and a voter identity
2. **No `unwrap()` in production code.** Use `?` and `anyhow::Result`
3. **Secrets never in logs.** RSA keys and `NOSTR_PRIVATE_KEY` stay in `SecretString`; never call `.expose_secret()` outside specific call sites
4. **All voter↔EC messages use NIP-59 Gift Wrap.** No plaintext Nostr messages to/from voters
5. **DB writes to `registration_tokens`/`authorized_voters` must use transactions** with `rows_affected()` checks — race conditions break the protocol
6. **Use `tracing`** for all logging, never `println!` or `log::` macros
7. **`blind-rsa-signatures` 0.17.1 only** — nonces are `[u8; 32]`, not `BigUint`. Do not use `num-bigint-dig`
8. **`candidate_ids` in votes table is JSON TEXT array** (`[3]` or `[3,1,4,2]`) — never a single integer column
9. **`ElectionRules` loaded fresh from TOML** on each `AddElection` call, never cached or hardcoded
10. **`cargo clippy -- -D warnings` must pass clean** before any PR
