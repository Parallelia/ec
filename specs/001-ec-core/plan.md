# Technical Plan: EC Core Daemon

**Spec**: `001-ec-core`  
**Created**: 2026-03-10  
**Stack**: Rust 1.94.0 · nostr-sdk 0.44.1 · blind-rsa-signatures 0.17.1 · tonic 0.14.5 · sqlx 0.8.6 · SQLite · tokio 1.50

---

## Architecture Overview

```text
┌─────────────────────────────────────────────────────────────┐
│                    EC Daemon (single binary)                 │
│                                                             │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────────┐ │
│  │  Nostr       │  │  gRPC Admin  │  │  Scheduler        │ │
│  │  Listener    │  │  Server      │  │  (30s tick)       │ │
│  │  (Gift Wrap) │  │  :50051      │  │  Status transitions│ │
│  └──────┬───────┘  └──────┬───────┘  └────────┬──────────┘ │
│         │                 │                    │            │
│         └─────────────────┴────────────────────┘           │
│                           │                                 │
│                    ┌──────▼──────┐                          │
│                    │  AppState   │                          │
│                    │  Arc<>      │                          │
│                    └──────┬──────┘                          │
│                           │                                 │
│              ┌────────────┴────────────┐                   │
│              │                         │                   │
│       ┌──────▼──────┐          ┌───────▼──────┐            │
│       │  SQLite DB  │          │  Nostr Client│            │
│       │  (sqlx)     │          │  (publish)   │            │
│       └─────────────┘          └──────────────┘            │
└─────────────────────────────────────────────────────────────┘
```

---

## Directory Structure

```text
ec/
├── Cargo.toml
├── build.rs                    # tonic protobuf compilation
├── proto/
│   └── admin.proto             # gRPC service definition
├── migrations/
│   ├── 001_initial.sql         # elections, candidates, voters, tokens, nonces, votes
│   └── 002_election_keys.sql   # per-election RSA keypair storage
├── rules/                      # bundled election rule files (TOML)
│   ├── plurality.toml          # Simple plurality / FPTP
│   └── stv.toml                # Single Transferable Vote
├── src/
│   ├── main.rs                 # startup, CLI args, spawn tasks
│   ├── config.rs               # settings from env/file (keys, relay URL, gRPC addr)
│   ├── state.rs                # AppState struct (shared Arc<>)
│   ├── db.rs                   # all SQLite queries (no ORM, raw sqlx)
│   ├── types.rs                # Election, Candidate, RegistrationToken, Vote, Ballot enums/structs
│   ├── crypto.rs               # RSA keypair gen, blind_sign(), verify_signature()
│   ├── rules/
│   │   ├── mod.rs              # load_rules(rules_id) -> Result<ElectionRules>
│   │   └── types.rs            # ElectionRules, BallotMethod, CountingAlgorithmId (serde TOML)
│   ├── counting/
│   │   ├── mod.rs              # CountingAlgorithm trait + registry (algorithm_for(id))
│   │   ├── plurality.rs        # PluralityAlgorithm: impl CountingAlgorithm
│   │   └── stv.rs              # StvAlgorithm: impl CountingAlgorithm
│   ├── nostr/
│   │   ├── mod.rs
│   │   ├── listener.rs         # Gift Wrap subscription + dispatch
│   │   ├── publisher.rs        # publish_election_event(), publish_result_event()
│   │   └── messages.rs         # inbound/outbound message types (serde)
│   ├── handlers/
│   │   ├── mod.rs
│   │   ├── register.rs         # handle "register" action
│   │   ├── request_token.rs    # handle "request-token" action
│   │   └── cast_vote.rs        # handle "cast-vote" — validates ballot vs rules
│   ├── grpc/
│   │   ├── mod.rs
│   │   ├── server.rs           # tonic service impl
│   │   └── admin.rs            # AdminService handlers (AddElection now takes rules_id)
│   └── scheduler.rs            # status transitions + invoke counting at election close
└── tests/
    ├── crypto_test.rs               # blind sign/verify roundtrip
    ├── registration_test.rs         # token generate/register flow
    ├── voting_test.rs               # full vote flow (blind → cast → tally)
    ├── double_vote_test.rs          # nonce reuse prevention
    ├── counting_plurality_test.rs   # PluralityAlgorithm unit tests
    ├── counting_stv_test.rs         # StvAlgorithm unit tests
    └── ballot_validation_test.rs    # ballot rejected when violates rules
```

---

## Database Schema

```sql
-- migrations/001_initial.sql

CREATE TABLE elections (
    id TEXT PRIMARY KEY NOT NULL,         -- nanoid
    name TEXT NOT NULL,
    start_time INTEGER NOT NULL,          -- unix timestamp
    end_time INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',  -- open | in_progress | finished | cancelled
    rules_id TEXT NOT NULL,               -- references a file in rules/{rules_id}.toml
    rsa_pub_key TEXT NOT NULL,            -- DER base64
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE candidates (
    id INTEGER NOT NULL,                  -- u8, candidate number within election
    election_id TEXT NOT NULL REFERENCES elections(id),
    name TEXT NOT NULL,
    PRIMARY KEY (id, election_id)
);

CREATE TABLE registration_tokens (
    token TEXT PRIMARY KEY NOT NULL,      -- random base64url, 32 bytes
    election_id TEXT NOT NULL REFERENCES elections(id),
    used INTEGER NOT NULL DEFAULT 0,      -- 0 = unused, 1 = used
    voter_pubkey TEXT,                    -- hex pubkey, set when token is consumed
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    used_at INTEGER
);

CREATE TABLE authorized_voters (
    voter_pubkey TEXT NOT NULL,           -- hex nostr pubkey
    election_id TEXT NOT NULL REFERENCES elections(id),
    registered_at INTEGER NOT NULL DEFAULT (unixepoch()),
    token_issued INTEGER NOT NULL DEFAULT 0,  -- 1 = already got blind sig, can't get another
    PRIMARY KEY (voter_pubkey, election_id)
);

CREATE TABLE used_nonces (
    h_n TEXT NOT NULL,                    -- SHA256(nonce) hex
    election_id TEXT NOT NULL REFERENCES elections(id),
    recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (h_n, election_id)
);

CREATE TABLE votes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    election_id TEXT NOT NULL REFERENCES elections(id),
    -- JSON array of candidate ids, ordered by voter preference.
    -- Plurality example: [3]
    -- STV example:       [3, 1, 4, 2]
    -- Stored as TEXT to support both single-choice and ranked ballots.
    -- NO voter identity. Ever.
    candidate_ids TEXT NOT NULL,
    recorded_at INTEGER NOT NULL DEFAULT (unixepoch())
);
```

```sql
-- migrations/002_election_keys.sql

CREATE TABLE IF NOT EXISTS election_keys (
    election_id TEXT PRIMARY KEY NOT NULL REFERENCES elections(id) ON DELETE CASCADE,
    rsa_priv_key TEXT NOT NULL,           -- DER base64
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
```

---

## Nostr Message Protocol

All messages are JSON, wrapped in NIP-59 Gift Wrap.

### Voter → EC

```json
// Register (request_id is optional in every action; the EC echoes it in the reply)
{ "action": "register", "election_id": "abc123", "registration_token": "base64...", "request_id": "hex..." }

// Request blind-signed token
{ "action": "request-token", "election_id": "abc123", "blinded_nonce": "base64..." }

// Cast vote — plurality (from anonymous keypair)
{ "action": "cast-vote", "election_id": "abc123", "candidate_ids": [1], "h_n": "hex...", "token": "base64..." }

// Cast vote — STV ranked ballot (from anonymous keypair)
{ "action": "cast-vote", "election_id": "abc123", "candidate_ids": [3, 1, 4, 2], "h_n": "hex...", "token": "base64..." }
```

### EC → Voter (replies via Gift Wrap to sender's pubkey)

```json
// Success (request_id echoed when the request carried one)
{ "status": "ok", "action": "register-confirmed", "request_id": "hex..." }
{ "status": "ok", "action": "token-issued", "blind_signature": "base64..." }
{ "status": "ok", "action": "vote-recorded" }

// Errors (request_id echoed when the request carried one)
{ "status": "error", "code": "INVALID_TOKEN", "message": "...", "request_id": "hex..." }
{ "status": "error", "code": "ALREADY_REGISTERED", "message": "..." }
{ "status": "error", "code": "NOT_AUTHORIZED", "message": "..." }
{ "status": "error", "code": "NONCE_ALREADY_USED", "message": "..." }
{ "status": "error", "code": "ELECTION_CLOSED", "message": "..." }
{ "status": "error", "code": "INVALID_CANDIDATE", "message": "..." }
{ "status": "error", "code": "BALLOT_INVALID", "message": "..." }
{ "status": "error", "code": "UNKNOWN_RULES", "message": "..." }
```

---

## gRPC Proto Definition

```protobuf
// proto/admin.proto
syntax = "proto3";
package admin;

service Admin {
  rpc AddElection (AddElectionRequest) returns (ElectionResponse);
  rpc AddCandidate (AddCandidateRequest) returns (CandidateResponse);
  rpc CancelElection (ElectionIdRequest) returns (StatusResponse);
  rpc GetElection (ElectionIdRequest) returns (ElectionResponse);
  rpc ListElections (Empty) returns (ElectionListResponse);
  rpc GenerateRegistrationTokens (GenerateTokensRequest) returns (TokensResponse);
  rpc ListRegistrationTokens (ElectionIdRequest) returns (TokenListResponse);
}

message AddElectionRequest {
  string name = 1;
  int64 start_time = 2;
  int64 end_time = 3;
  string rules_id = 4;   // e.g. "plurality" or "stv"
}

message GenerateTokensRequest {
  string election_id = 1;
  uint32 count = 2;
}

message TokensResponse {
  repeated string tokens = 1;   // base64-encoded tokens to distribute
}

message TokenListResponse {
  repeated TokenInfo tokens = 1;
}

message TokenInfo {
  string token_id = 1;   // truncated/hashed for display — not the raw token
  bool used = 2;
}
```

---

## Pluggable Counting Engine

### The `CountingAlgorithm` Trait

```rust
// src/counting/mod.rs

/// A single ballot as stored in SQLite: ordered list of candidate IDs.
/// Plurality: vec![1]
/// STV:       vec![3, 1, 4, 2]
pub type Ballot = Vec<u8>;

/// The result of counting all ballots for an election.
pub struct CountResult {
    /// Elected candidate IDs, in order of election (for STV: order matters).
    pub elected: Vec<u8>,
    /// Full per-candidate vote totals or final transfer tallies.
    pub tally: Vec<CandidateTally>,
    /// Optional: serialized count sheet for STV (one entry per round).
    pub count_sheet: Option<Vec<CountRound>>,
}

pub struct CandidateTally {
    pub candidate_id: u8,
    pub votes: f64,   // f64 for STV fractional transfer values; integer for plurality
    pub status: CandidateStatus,  // Elected | Excluded | Active
}

pub struct CountRound {
    pub round: u32,
    pub tallies: Vec<CandidateTally>,
    pub action: String,  // e.g. "Elected: Alice (surplus)", "Excluded: Bob"
}

pub trait CountingAlgorithm: Send + Sync {
    fn count(&self, ballots: &[Ballot], rules: &ElectionRules) -> anyhow::Result<CountResult>;
}

/// Registry: given a rules_id string, return the correct algorithm.
pub fn algorithm_for(rules_id: &str) -> anyhow::Result<Box<dyn CountingAlgorithm>> {
    match rules_id {
        "plurality" => Ok(Box::new(plurality::PluralityAlgorithm)),
        "stv"       => Ok(Box::new(stv::StvAlgorithm)),
        other       => anyhow::bail!("UNKNOWN_RULES: {}", other),
    }
}
```

### ElectionRules (deserialized from TOML)

```rust
// src/rules/types.rs

#[derive(Debug, Clone, Deserialize)]
pub struct ElectionRules {
    pub meta: RulesMeta,
    pub election: ElectionConfig,
    pub ballot: BallotConfig,
    pub counting: CountingConfig,
    pub results: ResultsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BallotConfig {
    pub method: BallotMethod,   // "single" | "ranked" | "approval"
    pub min_choices: u8,
    pub max_choices: u8,        // 0 = unlimited
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BallotMethod { Single, Ranked, Approval }
```

### Rules Loading

```rust
// src/rules/mod.rs
pub fn load_rules(rules_id: &str, rules_dir: &Path) -> anyhow::Result<ElectionRules> {
    let path = rules_dir.join(format!("{}.toml", rules_id));
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Rules file not found: {}", path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("Failed to parse rules file: {}", path.display()))
}
```

### Ballot Validation (in `handlers/cast_vote.rs`)

```rust
fn validate_ballot(candidate_ids: &[u8], rules: &ElectionRules, election_candidates: &[u8]) -> anyhow::Result<()> {
    let n = candidate_ids.len() as u8;
    if n < rules.ballot.min_choices {
        anyhow::bail!("BALLOT_INVALID: too few choices ({n}, min {})", rules.ballot.min_choices);
    }
    if rules.ballot.max_choices > 0 && n > rules.ballot.max_choices {
        anyhow::bail!("BALLOT_INVALID: too many choices ({n}, max {})", rules.ballot.max_choices);
    }
    for &id in candidate_ids {
        if !election_candidates.contains(&id) {
            anyhow::bail!("INVALID_CANDIDATE: {id}");
        }
    }
    if rules.ballot.method == BallotMethod::Ranked {
        let mut seen = std::collections::HashSet::new();
        for &id in candidate_ids {
            if !seen.insert(id) {
                anyhow::bail!("BALLOT_INVALID: duplicate candidate {id} in ranked ballot");
            }
        }
    }
    Ok(())
}
```

### Counting at Election Close (in `scheduler.rs`)

```rust
let rules = load_rules(&election.rules_id, &config.rules_dir)?;
let algorithm = algorithm_for(&election.rules_id)?;
let ballots: Vec<Ballot> = db::get_votes_for_election(&pool, &election.id).await?;
let result = algorithm.count(&ballots, &rules)?;
nostr::publisher::publish_result_event(&nostr_client, &election, &result).await?;
db::store_result(&pool, &election.id, &result).await?;
```

---

## Config Architecture

### Principle: Hybrid Config with Priority Layers

```text
env var  >  ec.toml  >  hardcoded default
```

- **Secrets** (private keys, passwords) → **env vars only**. Never in files that could end up in git, logs, or backups.
- **Non-secret config** (relay URL, ports, paths, timeouts) → **`ec.toml`** file. Versionable, readable, operator-friendly.
- **Development** → `.env` file loaded via `dotenvy` (never committed — in `.gitignore`).
- **Production** → systemd `EnvironmentFile=` or a secret manager.

### Additional Dependencies for Config

```toml
secrecy  = { version = "0.10", features = ["serde"] }   # wrap secrets in memory
dotenvy  = "0.15"                                        # load .env in development
```

Add these to the `[dependencies]` section of `Cargo.toml`.

### `ec.toml` — Non-secret configuration (committed to repo as example)

```toml
# ec.toml — operator configuration, safe to version control
# No secrets here. Ever.

relay_url  = "wss://relay.mostro.network"
grpc_bind  = "127.0.0.1:50051"
rules_dir  = "./rules"
log_level  = "info"
db_path    = "./ec.db"
```

### `.env.example` — Secret variable template (committed, but `.env` is gitignored)

```bash
# Copy to .env and fill in your values. NEVER commit .env.

# Nostr identity for the EC daemon (hex private key)
NOSTR_PRIVATE_KEY=your_hex_private_key_here

# Optional: password to encrypt per-election RSA keys stored in the database (not yet implemented)
# EC_DB_PASSWORD=

# Optional: override any ec.toml value via env (RELAY_URL, GRPC_BIND, etc.)
# RELAY_URL=wss://relay.mostro.network
```

### `config.rs` — Implementation

```rust
use secrecy::{ExposeSecret, SecretString};
use std::path::PathBuf;

#[derive(Debug)]
pub struct Config {
    // --- From ec.toml (non-secret) ---
    pub relay_url: String,
    pub grpc_bind: String,
    pub rules_dir: PathBuf,
    pub log_level: String,
    pub db_path: String,

    // --- From env vars (secrets) ---
    pub nostr_private_key: SecretString,   // never logged, never cloned carelessly
    pub db_password: Option<SecretString>, // optional encryption for per-election RSA keys
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        // 1. Load .env if present (dev convenience)
        let _ = dotenvy::dotenv();

        // 2. Load ec.toml if present, else use defaults
        let file_config = Self::load_toml("ec.toml").unwrap_or_default();

        // 3. Env vars override file config
        Ok(Config {
            relay_url: std::env::var("RELAY_URL")
                .unwrap_or(file_config.relay_url),
            grpc_bind: std::env::var("GRPC_BIND")
                .unwrap_or(file_config.grpc_bind),
            rules_dir: PathBuf::from(
                std::env::var("RULES_DIR").unwrap_or(file_config.rules_dir)
            ),
            log_level: std::env::var("LOG_LEVEL")
                .unwrap_or(file_config.log_level),
            db_path: std::env::var("DATABASE_URL")
                .unwrap_or(file_config.db_path),

            // Secrets: env vars only, required
            nostr_private_key: SecretString::new(
                std::env::var("NOSTR_PRIVATE_KEY")
                    .context("NOSTR_PRIVATE_KEY env var is required")?
            ),
            db_password: std::env::var("EC_DB_PASSWORD")
                .ok()
                .map(SecretString::new),
        })
    }
}
```

### `.gitignore` additions

```gitignore
# Secrets — never commit
.env
*.pem
ec.db
ec.db-*
```

---

## Key Implementation Notes

### Shared State

```rust
// src/state.rs
pub struct AppState {
    pub db: SqlitePool,
    pub nostr_client: Client,      // nostr-sdk Client
    pub ec_nostr_keys: Keys,       // EC's Nostr identity
    pub config: Config,
}
pub type SharedState = Arc<AppState>;
```

`SqlitePool` is `Clone + Send + Sync`. No `Mutex` needed for DB.

### RSA Key per Election

Each election gets its own RSA keypair at creation. Public key published in Kind 35000. Private key stored in `election_keys` table (DER base64, optionally encrypted with `EC_DB_PASSWORD` env var). On restart, keys are re-loaded from DB.

### Token Atomicity

```sql
-- In a transaction:
UPDATE registration_tokens
SET used = 1, voter_pubkey = ?1, used_at = unixepoch()
WHERE token = ?2 AND used = 0;
-- rows_affected() must == 1, else TOKEN_ALREADY_USED
```

### Nonce format (blind-rsa-signatures 0.17.1)

```rust
// Nonce is [u8; 32], NOT BigUint (that was 0.15.2)
use rand::RngCore;
let mut nonce = [0u8; 32];
rand::thread_rng().fill_bytes(&mut nonce);
let h_n = sha2::Sha256::digest(&nonce); // -> [u8; 32]
```

---

## Cargo.toml Dependencies

```toml
[package]
name = "ec"
version = "0.1.0"
edition = "2024"
rust-version = "1.94"

[dependencies]
tokio          = { version = "1.50",   features = ["macros", "rt-multi-thread"] }
anyhow         = "1.0"
serde          = { version = "1.0",    features = ["derive"] }
serde_json     = "1.0"
nostr-sdk      = { version = "0.44.1", features = ["nip59"] }
blind-rsa-signatures = "0.17.1"
base64         = "0.22"
nanoid         = "0.4"
sqlx           = { version = "0.8.6",  features = ["runtime-tokio-rustls", "sqlite", "macros", "chrono"] }
tonic          = "0.14.5"
prost          = "0.14.3"
chrono         = "0.4"
tracing        = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
rand           = "0.10"
clap           = { version = "4.5",    features = ["derive"] }
sha2           = "0.11.0-rc.5"        # rc required — blind-rsa-signatures 0.17.1 needs digest ^0.11
hex            = "0.4"
uuid           = { version = "1",      features = ["v4"] }
toml           = "1.0"
secrecy        = { version = "0.10", features = ["serde"] }
dotenvy        = "0.15"

[build-dependencies]
tonic-build = "0.14.5"

[dev-dependencies]
tempfile    = "3"
tokio-test  = "0.4"
```

> **Note on sha2 0.11-rc**: `blind-rsa-signatures 0.17.1` depends on `digest ^0.11`.
> `sha2 0.10.x` uses `digest 0.10`, which is incompatible. `sha2 0.11.0-rc.5` is the
> current release from the RustCrypto team and is functionally stable — the "rc" label
> reflects their release process, not instability. This is the correct dependency.

---

## Tasks

### Phase 1 — Foundation
- [x] Initialize Rust project, Cargo.toml (versions above), `.sqlx/` offline mode
- [x] Copy `rules/plurality.toml` and `rules/stv.toml` into repo
- [x] Write SQLite migrations (`001_initial.sql`) — include `rules_id` in elections, `candidate_ids TEXT` in votes
- [x] Implement `types.rs` structs with serde (include `Ballot = Vec<u8>`)
- [x] Implement `config.rs` — hybrid config system (see Config Architecture below)
- [x] Add `ec.toml` example file to repo root (non-secret defaults)
- [x] Add `.env.example` to repo root (secret vars template, never commit `.env`)
- [x] Implement `state.rs` AppState + SharedState
- [x] Implement `db.rs` with all query functions
- [x] Write `main.rs` startup (DB connect + migrations, Nostr client init, AppState, tracing)
- [x] **Verify `cargo build` passes clean**

### Phase 2 — Rules & Counting Engine
- [x] Implement `rules/types.rs`: `ElectionRules` and all sub-structs (serde + toml)
- [x] Implement `rules/mod.rs`: `load_rules(rules_id, rules_dir) -> Result<ElectionRules>`
- [x] Implement `counting/mod.rs`: `CountingAlgorithm` trait, `CountResult`, `algorithm_for()`
- [x] Implement `counting/plurality.rs`: `PluralityAlgorithm` — count single-choice ballots
- [x] Implement `counting/stv.rs`: `StvAlgorithm` — weighted inclusive Gregory, Droop quota
- [x] Write `tests/counting_plurality_test.rs`: 5 ballots, verify winner
- [x] Write `tests/counting_stv_test.rs`: 10 ranked ballots, 2 seats, verify elected

### Phase 3 — Cryptography
- [x] Implement `crypto.rs`: `generate_keypair()`, `blind_sign()`, `verify_signature()`
- [x] Nonce is `[u8; 32]` (rand 0.10), NOT BigUint
- [x] Write crypto roundtrip integration test

### Phase 4 — Nostr
- [x] Implement `nostr/publisher.rs`: `publish_election_event()`, `publish_result_event()`
- [x] Implement `nostr/listener.rs`: Gift Wrap subscription, message dispatch
- [x] Implement `nostr/messages.rs`: inbound/outbound types (`candidate_ids` as array)

### Phase 5 — Message Handlers
- [x] Implement `handlers/register.rs` (atomic token consumption)
- [x] Implement `handlers/request_token.rs` (blind sign + mark token issued)
- [x] Implement `handlers/cast_vote.rs` (validate ballot vs rules + verify token + nonce + record)
- [x] Write `tests/ballot_validation_test.rs`
- [x] Write integration tests for all 3 handlers

### Phase 6 — gRPC Admin API
- [x] Write `proto/admin.proto` (`AddElection` includes `rules_id`)
- [x] Configure `build.rs` for tonic-build
- [x] Implement `grpc/admin.rs`: all service methods
- [x] Implement `GenerateRegistrationTokens`

### Phase 7 — Scheduler
- [x] Implement `scheduler.rs`: 30s tick, status transitions
- [x] On `Finished`: load rules → `algorithm_for(rules_id)?.count()` → publish result

### Phase 8 — Polish
- [x] `tracing` instrumentation throughout
- [x] `cargo clippy --all-targets --all-features -- -D warnings` clean
- [x] README
- [x] Docker Compose (ec + nostr relay)
