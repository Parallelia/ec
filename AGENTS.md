# AGENTS.md — Parallelia EC

## Project

Electoral Commission daemon for the Parallelia trustless voting system.
See `.specify/memory/constitution.md` for governing principles.
See `.specify/specs/001-ec-core.md` for the feature specification.
See `.specify/specs/001-ec-core-plan.md` for the technical plan.

## Build Commands

```sh
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt
sqlx migrate run           # run SQLite migrations
```

## Rust & Dependency Versions

- **Rust**: 1.94.0 (stable, edition 2024)
- **blind-rsa-signatures**: 0.17.1 (nonce is `[u8; 32]`, NOT BigUint)
- **sha2**: 0.11.0-rc.5 (rc required — blind-rsa 0.17.1 needs digest ^0.11)
- **rand**: 0.10.0
- **nostr-sdk**: 0.44.1
- **sqlx**: 0.8.6 (stable — do NOT use 0.9.x alpha)
- **tonic/tonic-build**: 0.14.5
- **prost**: 0.14.3
- **secrecy**: 0.10 (wrap all secret values in `SecretString`)
- **dotenvy**: 0.15 (load `.env` in development)

## Key Rules for AI Agents

1. **Never compromise voter anonymity.** The EC must never store a link between a vote and a voter identity. See Principle 1 in the constitution.
2. **Use `blind-rsa-signatures = "0.17.1"`.** Version 0.15.2 does not compile on Rust 1.86+ (rsa 0.8 derive bug). The API change: nonces are `[u8; 32]` not `BigUint`. Do not use `num-bigint-dig` for nonces.
3. **All DB writes that touch `registration_tokens` or `authorized_voters` MUST be wrapped in a transaction** with `rows_affected()` checks. Race conditions here break the protocol.
4. **No `unwrap()` in production code paths.** Use `?` and `anyhow::Result`.
5. **All voter↔EC messages go through NIP-59 Gift Wrap.** No plaintext Nostr messages to/from voters.
6. **`tracing`** for all logging — not `println!`, not `log::info!`.
7. **Secrets never appear in logs.** RSA private keys, `NOSTR_PRIVATE_KEY`, and `EC_DB_PASSWORD` must be wrapped in `SecretString` from the `secrecy` crate. Never call `.expose_secret()` outside the specific call sites that need it. Never log them even at `tracing::debug!` level.
8. **Config uses the hybrid pattern**: non-secret values from `ec.toml` (with env var override), secrets from env vars only. See "Config Architecture" in the plan. Load `.env` via `dotenvy::dotenv()` at startup for dev convenience.
9. **`.env`, `*.pem`, and `ec.db*` must be in `.gitignore`.** Commit `ec.toml` (no secrets) and `.env.example` (template only). Never commit the actual `.env`.
10. **`cargo clippy -- -D warnings` must pass clean** before any PR.
11. The gRPC admin API binds to `127.0.0.1` by default. External binding requires explicit `GRPC_BIND` env var or `ec.toml` override.
12. Follow the task phases in order: Foundation → Rules & Counting → Crypto → Nostr → Handlers → gRPC → Scheduler → Polish.
13. **Every counting algorithm MUST implement `CountingAlgorithm` trait** from `src/counting/mod.rs`. No ad-hoc counting logic outside this module.
14. **`ElectionRules` is loaded fresh from the `.toml` file** on each `AddElection` call (no caching). Never hardcode rule values in Rust code — always read from the loaded `ElectionRules` struct.
15. **Ballot validation happens in `handlers/cast_vote.rs`** before touching the DB. Use `validate_ballot()` against the election's loaded rules. A ballot that violates `min_choices`/`max_choices` or contains invalid candidate IDs MUST be rejected before any DB write.
16. **`candidate_ids` in the `votes` table is a JSON TEXT array** (`[3]` or `[3,1,4,2]`). Never use a single integer column for votes — it breaks STV ranked ballots.
17. Adding a new counting method = implement `CountingAlgorithm` + register in `algorithm_for()` + add a `.toml` in `rules/`. No other files need to change.
18. **Add language identifiers to fenced code blocks** (e.g. `` ```rust ``, `` ```sql ``, `` ```text ``) to satisfy markdown linting (MD040).
