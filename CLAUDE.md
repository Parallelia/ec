# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Parallelia EC is the Electoral Commission daemon for a trustless electronic voting system. It manages elections, issues anonymous voting tokens via blind RSA signatures, communicates with voters over Nostr (NIP-59 Gift Wrap), and publishes verifiable results. **Experimental, unaudited software.**

## Build & Test Commands

```sh
cargo build                                          # build
cargo test                                           # run all tests (159, no external services needed)
cargo test --test crypto_test                        # run a single test file
cargo test test_crypto_roundtrip                     # run a single test
cargo clippy --all-targets --all-features -- -D warnings  # lint (must pass clean)
cargo fmt                                            # format
cargo fmt -- --check                                 # check formatting without modifying
cargo llvm-cov --summary-only                        # code coverage (requires cargo-llvm-cov + llvm-tools-preview)
cargo llvm-cov report --show-missing-lines           # list uncovered lines from the last run
```

Requires `protoc` (`apt install protobuf-compiler`) — the build script compiles `proto/admin/admin.proto`.

Tests live in `tests/` as integration tests (e.g. `tests/crypto_test.rs`, `tests/counting_plurality_test.rs`, `tests/counting_stv_test.rs`). They import from the `ec` library crate.

SQLite migrations run automatically at startup via `sqlx::migrate!("./migrations")`. For manual migration: `sqlx migrate run`.

### Test Infrastructure

- **`tests/common/mod.rs`** — shared helpers: `start_fake_relay()` (in-memory websocket Nostr relay speaking minimal NIP-01: `EVENT`/`OK`/`REQ`/`EOSE` + broadcast) and `init_tracing()` (TRACE subscriber so `tracing` macro bodies execute under coverage). Include with `mod common;`.
- **End-to-end tests use the fake relay**: `nostr_listener_test.rs` runs the full voter protocol (register → request-token → cast-vote) over real Gift Wrap; `scheduler_test.rs` runs the real `scheduler::run()` loop; `nostr_publisher_test.rs` and `grpc_admin_test.rs` assert real publishes.
- **`main_binary_test.rs`** spawns the compiled daemon (`CARGO_BIN_EXE_ec`) and drives it to controlled error exits — never kill it with a signal, or coverage data is lost.
- **`config_test.rs`** mutates env vars: all its tests must hold the shared `ENV_LOCK` mutex (test binaries run sequentially, so other files are safe).
- Handler tests assert that DB/crypto errors map to `INTERNAL_ERROR` and are never relayed to voters as protocol codes.

### Code Coverage

Measured with `cargo-llvm-cov` (install: `rustup component add llvm-tools-preview && cargo install cargo-llvm-cov`). Current baseline: **98.2% lines / 96.9% functions / 95.2% regions** — keep new code at or above this bar. The uncovered remainder is race-only branches, defensive unreachable code, and `tracing` macro attribution artifacts; don't chase those.

## Architecture

Single Rust binary (`src/main.rs`) with three concurrent surfaces:

- **Nostr listener/publisher** — voter communication via NIP-59 Gift Wrap (nostr-sdk 0.44.1)
- **gRPC admin API** — operator interface (tonic 0.14.5, proto at `proto/admin/admin.proto`, optional bearer auth via `EC_ADMIN_TOKEN`, optional TLS via `tls_cert`/`tls_key` in `ec.toml` or `TLS_CERT`/`TLS_KEY`)
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
- Secrets: env vars only, wrapped in `SecretString`. Required: `NOSTR_PRIVATE_KEY`. Optional: `EC_DB_PASSWORD`, `EC_ADMIN_TOKEN` (bearer auth for the gRPC admin API; empty value = disabled)
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
