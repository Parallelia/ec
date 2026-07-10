# Parallelia EC — Electoral Commission Daemon

> **EXPERIMENTAL SOFTWARE — NOT FOR PRODUCTION USE.**
> This code has **not** been audited and **must not** be used for real-world elections or any scenario where safety, legality, or physical risk is at stake.

## Overview

Parallelia EC (`ec`) is the **Electoral Commission daemon** for the Parallelia voting system — an experimental, trustless, open-source electronic voting platform built in Rust.

It is responsible for:

- **Managing elections** — create, configure, and monitor elections via a gRPC admin API
- **Issuing anonymous voting tokens** — blind RSA signatures (RFC 9474) ensure the EC cannot link a vote to a voter
- **Receiving votes over Nostr** — all voter communication uses NIP-59 Gift Wrap (encrypted, anonymous)
- **Publishing verifiable results** — election announcements (Kind 35000) and results (Kind 35001) are published to Nostr relays
- **Pluggable counting** — plurality (FPTP) and STV (Single Transferable Vote) built-in, extensible via trait

## Quick Start

### Prerequisites

- Rust toolchain 1.94+ (via [rustup](https://rustup.rs))
- Protocol Buffers compiler (`protoc`)
- SQLite development libraries
- [`grpcurl`](https://github.com/fullstorydev/grpcurl) (optional, for managing elections from the CLI)

```bash
# Ubuntu / Debian
sudo apt update
sudo apt install -y cmake build-essential libsqlite3-dev pkg-config libssl-dev protobuf-compiler ca-certificates
```

### Build

```bash
git clone https://github.com/parallelia/ec.git
cd ec
cargo build
```

### Configure

```bash
cp .env.example .env
```

Edit `.env` and set at least:

```bash
NOSTR_PRIVATE_KEY=your_hex_nostr_private_key_here
```

The Nostr private key is the EC's identity on the network. Generate one with any Nostr key tool or use `openssl rand -hex 32` for testing.

Non-secret configuration lives in `ec.toml` (already included in the repo):

```toml
relay_url  = "wss://relay.mostro.network"
grpc_bind  = "127.0.0.1:50051"
rules_dir  = "./rules"
log_level  = "info"
db_path    = "./ec.db"
```

### Run

```bash
cargo run
```

On startup, the EC will:

1. Load `.env` (if present) and `ec.toml`
2. Create the SQLite database at `db_path` (if it doesn't exist) and run migrations
3. Connect to the configured Nostr relay
4. Start the **scheduler** (30-second tick for election status transitions and vote counting)
5. Start the **Nostr listener** (receives Gift Wrap messages from voters)
6. Bind the **gRPC admin API** on `grpc_bind` (default `127.0.0.1:50051`)

## How It Works — Blind RSA Signature Protocol

The core idea: the EC signs a voting token **without seeing its content** (blind signature). This makes it cryptographically impossible to link a vote back to the voter who requested the token.

```text
┌─────────────┐     gRPC      ┌─────────────────┐     Nostr      ┌─────────────┐
│   Operator  │──────────────►│       EC        │◄──────────────►│   Voters    │
│   (Admin)   │  Port 50051   │  (Electoral     │  NIP-59 Gift   │  (Clients)  │
│             │               │  Commission)    │  Wrap Events   │             │
└─────────────┘               └────────┬────────┘               └─────────────┘
                                       │
                              ┌────────┴────────┐
                              │                 │
                       ┌──────▼──────┐   ┌──────▼──────┐
                       │  SQLite DB  │   │ Nostr Relay │
                       │  (ec.db)    │   │ (publish)   │
                       └─────────────┘   └─────────────┘
```

### Step-by-step Protocol

**Phase 1 — Setup (Operator)**

1. **Create election** via gRPC `AddElection` — the EC generates a fresh **RSA 2048-bit keypair** (SHA-384, PSS, Randomized mode) for this election and publishes the election announcement to Nostr (Kind 35000) with the RSA public key.
2. **Add candidates** via gRPC `AddCandidate` — only while the election status is `open`.
3. **Generate registration tokens** via gRPC `GenerateRegistrationTokens` — the EC returns a list of opaque, single-use tokens. Distribute these to voters out-of-band (email, Signal, in person, etc.).

**Phase 2 — Registration (Voter → EC, via Nostr Gift Wrap)**

4. **Voter registers** by sending a `register` message with their `registration_token`. The EC verifies the token is valid and unused, marks it as consumed, and authorizes the voter's Nostr public key for this election. The EC replies with `register-confirmed`.

**Phase 3 — Token Issuance (Voter → EC, via Nostr Gift Wrap)**

5. **Voter generates a random nonce** — 32 random bytes (`[u8; 32]`). Computes `h_n = SHA-256(nonce)`.
6. **Voter blinds `h_n`** using the election's RSA public key (from the Kind 35000 event) to produce `blinded_nonce`. The blinding factor is kept secret by the voter.
7. **Voter sends `request-token`** with the `blinded_nonce` (base64-encoded).
8. **EC verifies** the voter is authorized and hasn't already received a token. EC **blind-signs** the blinded nonce with the election's RSA private key and returns the `blind_signature`. EC marks `token_issued = true` — the voter cannot request another token.

> **Key insight**: The EC signs the blinded message without knowing what it's signing. When the voter unblinds the signature, the EC cannot correlate the resulting token with the voter who requested it.

**Phase 4 — Voting (Anonymous Voter → EC, via Nostr Gift Wrap)**

9. **Voter unblinds** the signature using the stored blinding factor → valid anonymous voting token. Voter also obtains the `msg_randomizer` (32 bytes, per RFC 9474).
10. **Voter creates a fresh, anonymous Nostr keypair** — completely unlinked from their real identity.
11. **From the anonymous keypair**, voter sends a `cast-vote` message containing:
    - `candidate_ids` — the voter's choice(s): `[1]` for plurality, `[3, 1, 4, 2]` for ranked (STV)
    - `h_n` — hex-encoded SHA-256 of the original nonce
    - `token` — base64-encoded `signature ++ msg_randomizer` (signature bytes concatenated with 32-byte randomizer)
12. **EC verifies** the token signature against the election's RSA public key, checks that `h_n` has not been used before (prevents double voting), validates the ballot against the election's rules, and records the vote. **No voter identity is stored with the vote.**

**Phase 5 — Counting & Results**

13. The **scheduler** (30-second tick) monitors election timing. When `end_time` is reached, the election transitions to `finished`.
14. The EC loads the election rules, invokes the counting algorithm, and publishes the results to Nostr (Kind 35001).

## Creating and Managing Elections

The EC provides a gRPC admin API for election management. You can use [`grpcurl`](https://github.com/fullstorydev/grpcurl) or any gRPC client.

### 1. Create an Election

```bash
grpcurl -plaintext -import-path proto -proto admin/admin.proto \
  -d '{
    "name": "Council 2026",
    "start_time": 1711200000,
    "end_time": 1711300000,
    "rules_id": "plurality"
  }' \
  localhost:50051 proto.admin.Admin/AddElection
```

- `start_time` / `end_time` — Unix timestamps. The scheduler will transition the election to `in_progress` when `start_time` arrives and to `finished` when `end_time` arrives.
- `rules_id` — `"plurality"` or `"stv"`. Must match a `.toml` file in the `rules/` directory.

The response includes the `election_id` and the `rsa_pub_key` (base64 DER) generated for this election.

### 2. Add Candidates

Candidates can only be added while the election is `open`:

```bash
grpcurl -plaintext -import-path proto -proto admin/admin.proto \
  -d '{"election_id": "<ELECTION_ID>", "id": 1, "name": "Alice"}' \
  localhost:50051 proto.admin.Admin/AddCandidate
```

```bash
grpcurl -plaintext -import-path proto -proto admin/admin.proto \
  -d '{"election_id": "<ELECTION_ID>", "id": 2, "name": "Bob"}' \
  localhost:50051 proto.admin.Admin/AddCandidate
```

### 3. Generate Registration Tokens

Generate tokens to distribute to eligible voters:

```bash
grpcurl -plaintext -import-path proto -proto admin/admin.proto \
  -d '{"election_id": "<ELECTION_ID>", "count": 100}' \
  localhost:50051 proto.admin.Admin/GenerateRegistrationTokens
```

The response contains a list of opaque, single-use tokens. Distribute one per voter via a secure channel.

### 4. Monitor Elections

```bash
# List all elections
grpcurl -plaintext -import-path proto -proto admin/admin.proto \
  localhost:50051 proto.admin.Admin/ListElections

# Get a specific election
grpcurl -plaintext -import-path proto -proto admin/admin.proto \
  -d '{"election_id": "<ELECTION_ID>"}' \
  localhost:50051 proto.admin.Admin/GetElection

# List registration tokens and their usage status
grpcurl -plaintext -import-path proto -proto admin/admin.proto \
  -d '{"election_id": "<ELECTION_ID>"}' \
  localhost:50051 proto.admin.Admin/ListRegistrationTokens
```

### 5. Cancel an Election

```bash
grpcurl -plaintext -import-path proto -proto admin/admin.proto \
  -d '{"election_id": "<ELECTION_ID>"}' \
  localhost:50051 proto.admin.Admin/CancelElection
```

The full proto definition is at [`proto/admin/admin.proto`](proto/admin/admin.proto).

## Voter Flow — Nostr Message Protocol

All voter-EC communication is JSON inside NIP-59 Gift Wrap events (encrypted, sender-anonymous).

Every request may include an optional `request_id` (client-generated random string, max 64 bytes). The EC echoes it verbatim in the reply — success or error — so clients can match responses to in-flight requests and ignore Gift Wraps replayed by relays. Requests without a `request_id` get replies without one.

### Voter → EC

**Register** (use the token received from the operator):

```json
{
  "action": "register",
  "election_id": "abc123",
  "registration_token": "base64url_token_here",
  "request_id": "random_hex_string"
}
```

**Request blind-signed token** (after registration, send blinded nonce):

```json
{
  "action": "request-token",
  "election_id": "abc123",
  "blinded_nonce": "base64_blinded_hash_here"
}
```

**Cast vote** (from a fresh anonymous Nostr keypair):

```json
{
  "action": "cast-vote",
  "election_id": "abc123",
  "candidate_ids": [1],
  "h_n": "hex_sha256_of_nonce",
  "token": "base64_signature_and_msg_randomizer"
}
```

For ranked ballots (STV), `candidate_ids` is an ordered preference list: `[3, 1, 4, 2]`.

The `token` field is the base64-encoded concatenation of the unblinded RSA signature bytes followed by the 32-byte `msg_randomizer` (per RFC 9474 Randomized mode).

### EC → Voter

**Success responses:**

```json
{ "status": "ok", "action": "register-confirmed", "request_id": "echoed..." }
{ "status": "ok", "action": "token-issued", "blind_signature": "base64...", "request_id": "echoed..." }
{ "status": "ok", "action": "vote-recorded", "request_id": "echoed..." }
```

**Error responses:**

```json
{ "status": "error", "code": "ERROR_CODE", "message": "Human-readable description", "request_id": "echoed..." }
```

`request_id` is present only when the request carried one.

| Error Code | Meaning |
|---|---|
| `ELECTION_NOT_FOUND` | No election with that ID |
| `INVALID_TOKEN` | Registration token is invalid or already used |
| `ALREADY_REGISTERED` | Voter pubkey already registered for this election |
| `NOT_AUTHORIZED` | Voter pubkey not authorized (didn't register) |
| `NONCE_ALREADY_USED` | This `h_n` was already used (double vote attempt) |
| `ELECTION_CLOSED` | Election is not in the correct state for this action |
| `INVALID_CANDIDATE` | Candidate ID not found in this election |
| `BALLOT_INVALID` | Ballot violates rules (too few/many choices, duplicates, etc.) |
| `UNKNOWN_RULES` | The `rules_id` doesn't match any known counting algorithm |

## Election Status Flow

```text
  open ──────► in_progress ──────► finished
   │                                   ▲
   │                                   │
   └──► cancelled                      │
                                  (counting +
                                   publish results)
```

- **`open`** — Election created. Candidates can be added. Voters can register and request tokens.
- **`in_progress`** — `start_time` reached. Voters can cast votes. No more candidates can be added.
- **`finished`** — `end_time` reached. Votes counted, results published to Nostr (Kind 35001).
- **`cancelled`** — Election cancelled via admin API.

The scheduler checks every 30 seconds for status transitions.

## Nostr Events

### Kind 35000 — Election Announcement

Published when an election is created. Addressable event with `d` tag = election ID.

```json
{
  "election_id": "abc123",
  "name": "Council 2026",
  "start_time": 1711200000,
  "end_time": 1711300000,
  "status": "open",
  "rules_id": "plurality",
  "rsa_pub_key": "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A...",
  "candidates": [
    { "id": 1, "name": "Alice" },
    { "id": 2, "name": "Bob" }
  ]
}
```

The `rsa_pub_key` is the base64-encoded DER public key that voters use to blind their nonces.

### Kind 35001 — Election Results

Published when counting is complete. Addressable event with `d` tag = election ID.

```json
{
  "election_id": "abc123",
  "name": "Council 2026",
  "rules_id": "plurality",
  "elected": [1],
  "tally": [
    { "candidate_id": 1, "votes": 42.0, "status": "elected" },
    { "candidate_id": 2, "votes": 31.0, "status": "active" }
  ]
}
```

For STV elections, the result includes a `count_sheet` array with per-round tallies showing surplus transfers and exclusions.

## Election Rules

Election rules are defined as TOML files in the `rules/` directory. The `rules_id` passed to `AddElection` must match the filename (e.g., `"plurality"` loads `rules/plurality.toml`).

### Built-in Rules

| Rules ID | Algorithm | Ballot Type | Description |
|---|---|---|---|
| `plurality` | First Past the Post | Single choice (`[1]`) | N highest vote-getters win (configurable seats) |
| `stv` | Single Transferable Vote | Ranked (`[3,1,4,2]`) | Weighted Inclusive Gregory method, Droop quota |

### Adding a New Counting Algorithm

1. Implement the `CountingAlgorithm` trait in `src/counting/`
2. Register it in `algorithm_for()` in `src/counting/mod.rs`
3. Add a `.toml` file in `rules/` describing ballot format, seat count, and counting parameters

See `rules/plurality.toml` and `rules/stv.toml` for the full configuration schema.

## Configuration

### Precedence

```text
Environment variable  >  ec.toml  >  Hardcoded default
```

### Environment Variables

| Variable | Required | Description |
|---|---|---|
| `NOSTR_PRIVATE_KEY` | Yes | Hex-encoded Nostr private key for the EC identity |
| `EC_DB_PASSWORD` | No | Password to encrypt per-election RSA keys (not yet implemented) |
| `EC_ADMIN_TOKEN` | No | Bearer token for the gRPC admin API. When set, every call must carry `authorization: Bearer <token>`; unset or empty disables auth (safe only on loopback) |
| `RELAY_URL` | No | Overrides `relay_url` from `ec.toml` |
| `GRPC_BIND` | No | Overrides `grpc_bind` from `ec.toml` |
| `RULES_DIR` | No | Overrides `rules_dir` from `ec.toml` |
| `LOG_LEVEL` | No | Overrides `log_level` from `ec.toml` |
| `DATABASE_URL` | No | Overrides `db_path` from `ec.toml` |

Secrets are **never** stored in `ec.toml`. They are loaded from environment variables only and kept in memory as `SecretString`.

For local development, use `.env` (loaded by `dotenvy` at startup). `.env` is gitignored and must never be committed.

## Architecture

Single Rust binary with three concurrent surfaces:

| Surface | Purpose |
|---|---|
| **Nostr Listener** | Subscribes to NIP-59 Gift Wrap events, unwraps, dispatches to handlers, replies via Gift Wrap |
| **gRPC Admin API** | Operator interface for election management (local, non-voter) |
| **Scheduler** | 30-second tick loop — election status transitions, vote counting, result publishing |

### Module Layout

| Module | Purpose |
|---|---|
| `config.rs` | Hybrid config: `ec.toml` + env vars. Secrets in `SecretString` |
| `crypto.rs` | Blind RSA: keypair gen (2048-bit, SHA-384, PSS, Randomized), blind sign, verify |
| `db.rs` | All SQLite queries. Token + voter writes use transactions with `rows_affected()` checks |
| `types.rs` | Domain structs: Election, Candidate, RegistrationToken, AuthorizedVoter, Vote, UsedNonce |
| `state.rs` | `AppState` (db pool, nostr client, keys, config) shared via `Arc` |
| `rules/` | Election rule loading from TOML files. `ElectionRules` struct |
| `counting/` | `CountingAlgorithm` trait + implementations. `algorithm_for()` registry |
| `nostr/` | Listener (Gift Wrap subscription), publisher (Kind 35000/35001), message types |
| `handlers/` | Register, request-token, cast-vote handlers |
| `grpc/` | tonic service: AddElection, AddCandidate, GenerateTokens, etc. |
| `scheduler.rs` | Status transitions + counting at election close |

### Database

SQLite with 7 tables across 3 migrations. Key design decisions:

- **`votes` table stores NO voter identity** — only `election_id`, `candidate_ids` (JSON array), and `recorded_at`
- **Per-election RSA keypairs** stored in `election_keys` table (DER base64)
- **Registration token consumption** and **voter authorization** use transactions with `rows_affected()` checks to prevent race conditions

## Development

```bash
cargo build                                          # Build
cargo test                                           # Run all tests (159 tests)
cargo clippy --all-targets --all-features -- -D warnings  # Lint (must pass clean)
cargo fmt                                            # Format
cargo fmt -- --check                                 # Check formatting
```

### Testing

Tests live in `tests/` as integration tests (plus a few unit tests for private
helpers in `src/main.rs`). They use in-memory SQLite databases and an
**in-memory fake Nostr relay** (`tests/common/mod.rs`) — no external services
are required. The fake relay speaks just enough NIP-01 (`EVENT`/`OK`/`REQ`/`EOSE`
plus broadcast) to exercise the real networking paths.

| Test file | What it covers |
|---|---|
| `nostr_listener_test.rs` | Full voter flow end-to-end over NIP-59 Gift Wrap: register → request-token → cast-vote, `request_id` correlation, malformed messages, undecryptable wraps |
| `scheduler_test.rs` | The real `scheduler::run()` loop: status transitions, counting, result publishing, retry-on-failure, tick errors |
| `grpc_admin_test.rs` | Every admin RPC (happy paths, validation, path-traversal rejection, DB failures) |
| `nostr_publisher_test.rs` | Kind 35000/35001 events, with/without STV count sheet, relay failures |
| `handler_*_test.rs` | Register / request-token / cast-vote handlers, every protocol error code, no internal-error leakage to voters |
| `counting_*_test.rs`, `ballot_validation_test.rs` | Plurality + STV algorithms: surplus transfers, exclusions, all tie-breaking modes (`backwards`/`random`/`manual`), unsupported options |
| `db_test.rs`, `crypto_test.rs`, `rules_test.rs`, `config_test.rs` | Query layer, blind RSA error paths, rules loading, hybrid config precedence |
| `main_binary_test.rs` | Daemon startup smoke tests (config, migrations, Nostr client, gRPC bind) driving the real binary to controlled exits |
| `request_id_correlation_test.rs` | Reply correlation contract for `request_id` |

### Code Coverage

Coverage is measured with [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov)
(LLVM source-based instrumentation, the de-facto standard for Rust):

```bash
# One-time setup
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov

# Terminal summary
cargo llvm-cov --summary-only

# Browsable HTML report (target/llvm-cov/html/index.html)
cargo llvm-cov --html

# Show exactly which lines are uncovered
cargo llvm-cov report --show-missing-lines
```

Current status (`cargo llvm-cov --summary-only`):

| Metric | Coverage |
|---|---|
| **Lines** | **98.2%** |
| Functions | 96.9% |
| Regions | 95.2% |

Twelve of sixteen source files are at 100% line coverage (`config`, `counting/mod`,
`counting/plurality`, `crypto`, `db`, `handlers/*`, `nostr/messages`, `rules`, …).
The remaining ~1.8% consists of branches that only occur under race conditions
(e.g. two schedulers competing for the same transition), defensive unreachable
code in STV, and `tracing` macro instrumentation artifacts — verified to execute
but misattributed by the coverage tooling.

## Docker

A `docker-compose.yml` is included that runs the EC alongside a Nostr relay:

```bash
# Set your Nostr private key
export NOSTR_PRIVATE_KEY=your_hex_key_here

# Start EC + relay
docker compose up -d
```

This starts:
- **nostr-rs-relay** on port 8080
- **EC daemon** on port 50051 (gRPC), connected to the relay

Data is persisted in Docker volumes (`ec-data`, `relay-data`).

## Security Considerations

- **Voter anonymity** — Blind RSA signatures make it cryptographically impossible for the EC to link a vote to the voter who requested the token
- **gRPC binds localhost** by default — change `grpc_bind` only if you understand the risk
- **Secrets in env vars only** — private keys are wrapped in `SecretString` and never logged
- **Per-election RSA keypairs** — each election gets its own 2048-bit key, generated at creation
- **Double vote prevention** — nonce hashes (`h_n`) are tracked per election; reuse is rejected
- **NIP-59 Gift Wrap** — all voter-EC messages are encrypted and sender-anonymous on the wire

## Limitations

- **Experimental** — no formal security audit. Use only for research, testing, and education
- **Single EC** — central authority issues tokens. No threshold or multi-party setup
- **No voter client** — this repo is the EC only. The voter client is a separate project

## Tech Stack

- **Language**: Rust 1.94 (edition 2024)
- **Async runtime**: tokio 1.50
- **Nostr**: nostr-sdk 0.44.1 (NIP-59 Gift Wrap)
- **Blind signatures**: blind-rsa-signatures 0.17.1 (RFC 9474)
- **Database**: SQLite via sqlx 0.8.6
- **Admin API**: gRPC via tonic 0.14.5 / prost 0.14.3
- **Config**: toml, dotenvy, secrecy

## Related Projects

- [Criptocracia MVP](https://github.com/parallelia/criptocracia) — the original prototype (EC + voter client) that this project grew out of; since renamed **Parallelia**
- [Parallelia Voter](https://github.com/parallelia/voter) — the TUI voting client for this EC

**This repository is experimental and unaudited. Use it only for research, testing, and education.**

## License

This project is licensed under MIT. See [LICENSE](LICENSE) for details.
