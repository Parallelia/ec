# Feature Specification: EC Core Daemon

**Feature Branch**: `001-ec-core`  
**Created**: 2026-03-10  
**Status**: Draft  
**Repo**: https://github.com/parallelia/ec

---

## Context

This is a clean rewrite of the Electoral Commission daemon from the Criptocracia MVP
(`parallelia/criptocracia`). The MVP validated the cryptographic protocol
(blind RSA signatures over Nostr). This rewrite adds the **complete voter registration flow**
(MVP issues #2–#5), a **pluggable counting engine**, improves architecture, and separates
the EC from the voter client.

The voter client lives in a separate repo and is out of scope here.

### Pluggable Counting System

Elections are configured via `.toml` rule files. Each file declares the counting
`algorithm` to use. The EC loads the rule file at election creation time and dispatches
to the correct counting engine. This means:

- **New variant of an existing method** (e.g. STV with Irish rules) → new `.toml` only, no code change.
- **Genuinely new method** (e.g. Approval Voting, Borda Count) → new `.toml` + new Rust module implementing the `CountingAlgorithm` trait.

Bundled rule files (in `rules/`):
- `rules/plurality.toml` — Simple plurality / First Past the Post
- `rules/stv.toml` — Single Transferable Vote (Weighted Inclusive Gregory, Droop quota)

---

## User Scenarios & Testing

### User Story 1 — EC Operator Creates and Manages an Election (Priority: P1)

An operator starts the EC daemon, uses the gRPC admin API to create an election with
candidates, a time window, and a reference to a counting rule file (e.g. `plurality`
or `stv`). The EC loads the rule, validates it, and publishes the election to Nostr.

**Why this priority**: Without elections there is nothing to vote on. This is the foundation.

**Independent Test**: Run the daemon, call `AddElection` with `rules_id = "plurality"` and
`AddCandidate` via gRPC, verify a Kind 35000 Nostr event is published with correct election
metadata including the rules identifier.

**Acceptance Scenarios**:

1. **Given** the daemon is running, **When** the operator calls `AddElection` with name, start_time, end_time, and `rules_id` (e.g. `"plurality"`), **Then** a new election is created in SQLite with status `Open`, the rule file is loaded and validated, and a Kind 35000 event is published with `rules_id` in its content.
2. **Given** an `rules_id` that does not exist in the `rules/` directory, **When** the operator calls `AddElection`, **Then** the EC rejects the request with `UNKNOWN_RULES`.
3. **Given** an election exists in status `Open`, **When** the operator calls `AddCandidate`, **Then** the candidate is persisted and the Kind 35000 event is updated (replaced) with the new candidate list.
4. **Given** an election with `start_time` in the past, **When** the scheduler runs, **Then** the election transitions to `InProgress` and the Kind 35000 event is updated.
5. **Given** an election with `end_time` in the past, **When** the scheduler runs, **Then** the election transitions to `Finished`, the counting engine for that election's `rules_id` is invoked, the final result is published (Kind 35001), and no more votes are accepted.
6. **Given** a running election, **When** the operator calls `CancelElection`, **Then** status becomes `Cancelled` and the Kind 35000 event reflects the cancellation.

---

### User Story 2 — EC Operator Generates Voter Registration Tokens (Priority: P1)

The operator generates a batch of one-time registration tokens for eligible voters and
distributes them through an out-of-band channel (email, Signal, printed list, etc.).
The EC does NOT associate tokens with real-world identities.

**Why this priority**: This is the core missing piece from the MVP. Without it, the EC
admin must manually insert voter pubkeys, which breaks anonymity.

**Independent Test**: Call `GenerateRegistrationTokens(election_id, count=5)` via gRPC,
receive 5 opaque tokens, verify they are stored in SQLite as unused and not linked to
any pubkey yet.

**Acceptance Scenarios**:

1. **Given** an election in status `Open`, **When** the operator calls `GenerateRegistrationTokens(election_id, count=N)`, **Then** N unique opaque tokens (UUID or random bytes, base64-encoded) are generated, stored in SQLite with `used=false, pubkey=null`, and returned to the operator.
2. **Given** a token that has already been used, **When** a voter attempts to register with it again, **Then** the EC rejects the request with a `TOKEN_ALREADY_USED` error.
3. **Given** an election in status `InProgress` or `Finished`, **When** the operator tries to generate new tokens, **Then** the EC rejects the request — registration is only allowed while `Open`.
4. **Given** a batch of tokens generated, **When** the operator calls `ListRegistrationTokens(election_id)`, **Then** the EC returns token IDs with their `used` status (but NOT the pubkeys, to protect voter privacy).

---

### User Story 3 — Voter Registers Using a Token (Priority: P1)

A voter who received a registration token sends it to the EC along with their Nostr
pubkey. The EC validates the token, links the pubkey to the election's voter registry,
and marks the token as used. From this point on, the voter is eligible to request a
blind-signed voting token.

**Why this priority**: Registration is the gateway to voting. No registration = no vote.

**Independent Test**: Send a Gift Wrap message to the EC with action `register` and a
valid token. Verify the EC responds with confirmation and the voter's pubkey is added
to the election's authorized voter list.

**Acceptance Scenarios**:

1. **Given** a valid unused token and an election in status `Open`, **When** a voter sends `{action: "register", election_id, registration_token}` via NIP-59 Gift Wrap, **Then** the EC validates the token, adds the voter's Nostr pubkey to the authorized list for that election, marks the token as `used`, and replies with a confirmation Gift Wrap.
2. **Given** the voter's pubkey is already registered for that election, **When** they try to register again, **Then** the EC rejects with `ALREADY_REGISTERED`.
3. **Given** an invalid or nonexistent token, **When** a voter sends it, **Then** the EC rejects with `INVALID_TOKEN` and does not register the pubkey.
4. **Given** an election in status `InProgress` (voting has started), **When** a voter tries to register, **Then** the EC rejects with `REGISTRATION_CLOSED`.

---

### User Story 4 — Voter Requests a Blind-Signed Voting Token (Priority: P1)

A registered voter blinds a nonce, sends the blinded nonce to the EC, and receives a
blind signature. The voter unblinds it to get a valid voting token. The EC never sees
the nonce — it cannot link the token to this voter.

**Why this priority**: This is the core anonymization step. Without it there is no
anonymous voting.

**Independent Test**: Send a `request-token` Gift Wrap from a registered voter pubkey
with a valid blinded nonce. Verify the EC returns a blind signature and removes the
voter from the authorized list (preventing double token requests).

**Acceptance Scenarios**:

1. **Given** a registered voter and an election in status `InProgress`, **When** the voter sends `{action: "request-token", election_id, blinded_nonce}` via Gift Wrap, **Then** the EC signs the blinded nonce with its RSA private key, removes the voter's pubkey from the authorized list (one-time use), and replies with the blind signature.
2. **Given** a voter who has already requested a token (pubkey removed from authorized list), **When** they try to request again, **Then** the EC rejects with `NOT_AUTHORIZED`.
3. **Given** an election in status `Open` (voting not started yet), **When** a voter requests a token, **Then** the EC rejects with `VOTING_NOT_STARTED`.
4. **Given** an election in status `Finished` or `Cancelled`, **When** a voter requests a token, **Then** the EC rejects with `ELECTION_CLOSED`.

---

### User Story 5 — Voter Casts an Anonymous Vote (Priority: P1)

The voter sends their vote using a fresh anonymous Nostr keypair (not their registered
pubkey). The ballot format depends on the election's counting rule:
- **Plurality**: `candidate_ids` is a single-element list `[3]`
- **STV**: `candidate_ids` is an ordered preference list `[3, 1, 4, 2]`

The EC verifies the token, validates the ballot against the election's rules
(correct number of choices, valid candidate IDs), and records the vote.

**Why this priority**: This is the actual voting act. The anonymity is preserved because
the EC cannot link the fresh keypair to the registered voter.

**Independent Test**: Send a `cast-vote` Gift Wrap from an anonymous pubkey with a
valid unblinded token, nonce hash, and candidate_ids. Verify the vote is recorded,
the tally is updated (for plurality), and a Kind 35001 event is published.

**Acceptance Scenarios**:

1. **Given** a valid unblinded token and an election in status `InProgress`, **When** an anonymous voter sends `{action: "cast-vote", election_id, candidate_ids: [N, ...], h_n, token}` via Gift Wrap, **Then** the EC verifies the token signature, validates the ballot against the election rules (choice count, valid IDs), checks the nonce hash has not been used, records the vote, marks the nonce as used, and for `publish_tally = "live"` elections, publishes an updated Kind 35001 event.
2. **Given** a nonce hash that has already been used (double vote attempt), **When** the voter tries to cast again, **Then** the EC rejects with `NONCE_ALREADY_USED`.
3. **Given** an invalid token (bad signature), **When** the voter tries to cast, **Then** the EC rejects with `INVALID_TOKEN`.
4. **Given** a `candidate_ids` list containing an ID that does not exist in the election, **When** the voter casts, **Then** the EC rejects with `INVALID_CANDIDATE`.
5. **Given** a plurality election (max_choices = 1), **When** a voter sends `candidate_ids` with 2+ entries, **Then** the EC rejects with `BALLOT_INVALID` ("too many choices for this election's rules").
6. **Given** an STV election with `min_choices = 1`, **When** a voter sends an empty `candidate_ids` list, **Then** the EC rejects with `BALLOT_INVALID`.

---

### User Story 6 — EC Admin Queries Election State (Priority: P2)

The operator can query elections, voter counts, and current tallies via gRPC at any
time during the election lifecycle.

**Acceptance Scenarios**:

1. **Given** any election, **When** operator calls `GetElection(election_id)`, **Then** returns full election metadata including status, candidate list, and voter count.
2. **When** operator calls `ListElections`, **Then** returns all elections with their current status.
3. **When** operator calls `ListRegistrationTokens(election_id)`, **Then** returns token IDs and `used` status.

---

### Edge Cases

- What happens if the Nostr relay is unreachable when the EC tries to publish? → Log error, retry with exponential backoff, do not block the voting flow.
- What happens if the EC restarts mid-election? → Elections and voter state must be fully restored from SQLite on startup. The rule file for each election is re-loaded from `rules/` by `rules_id`.
- What happens if a rule file is deleted after an election was created with it? → The EC must log a critical error on startup and refuse to accept new votes for that election. Existing recorded votes are preserved in SQLite.
- What happens if a plurality voter sends a ranked ballot? → The EC validates ballot format against the loaded rules and rejects with `BALLOT_INVALID`.
- What happens if two voters register with the same token simultaneously? → SQLite unique constraint + transaction must prevent double-use. Last write wins is not acceptable.
- What happens if `start_time` is in the past when an election is created? → EC accepts it but immediately transitions to `InProgress` on next scheduler tick.
- What if a voter sends a registration request to the wrong election ID? → `ELECTION_NOT_FOUND` error.

---

## Requirements

### Functional Requirements

- **FR-001**: The EC MUST support multiple concurrent elections.
- **FR-002**: The EC MUST enforce blind RSA signatures using `blind-rsa-signatures = "0.15.2"` with RSA keys of at least 2048 bits.
- **FR-003**: The EC MUST generate per-election RSA keypairs (not shared across elections).
- **FR-004**: The EC MUST expose a gRPC admin API on configurable address (default `127.0.0.1:50051`).
- **FR-005**: The EC MUST persist all state (elections, candidates, voters, tokens, used nonces, votes) in SQLite using `sqlx`.
- **FR-006**: The EC MUST restore complete state from SQLite on startup with no data loss.
- **FR-007**: The EC MUST publish election events (Kind 35000) and tally/result events (Kind 35001) to Nostr relays using `nostr-sdk = "0.41"` with NIP-59.
- **FR-008**: All voter↔EC messages MUST travel via NIP-59 Gift Wrap. The EC MUST ignore any non-Gift-Wrap messages from voter pubkeys.
- **FR-009**: The EC MUST generate registration tokens on demand (gRPC `GenerateRegistrationTokens`).
- **FR-010**: Registration tokens MUST be single-use. SQLite MUST enforce this with a unique constraint and a transaction-level check.
- **FR-011**: The EC MUST remove a voter's pubkey from the authorized list after they request a blind-signed token (one token per voter per election, non-transferable).
- **FR-012**: The EC MUST track used nonce hashes per election to prevent double voting.
- **FR-013**: Election status transitions MUST be automatic, driven by a scheduler running every 30 seconds.
- **FR-014**: The EC MUST log all significant events using `tracing` with configurable log level.
- **FR-015**: RSA private keys MUST be loadable from environment variables OR from PEM files. They MUST NOT be hardcoded.
- **FR-016**: Each election MUST reference a `rules_id` that maps to a `.toml` file in the `rules/` directory. The EC MUST reject election creation if the `rules_id` is unknown.
- **FR-017**: The EC MUST implement a `CountingAlgorithm` trait with the signature `fn count(&self, ballots: &[Ballot], rules: &ElectionRules) -> CountResult`. Each counting method (plurality, STV) MUST implement this trait independently.
- **FR-018**: The EC MUST validate each incoming ballot against the election's loaded `ElectionRules` before recording it (correct number of choices per `min_choices`/`max_choices`, all `candidate_ids` valid for the election).
- **FR-019**: The `Vote` table MUST store `candidate_ids` as a JSON array (TEXT column) to support both single-choice and ranked ballots in the same schema.
- **FR-020**: When an election finishes, the EC MUST invoke the appropriate `CountingAlgorithm::count()` with all recorded ballots and publish the result via Kind 35001. For `publish_tally = "live"` elections (plurality), intermediate tallies MAY be published after each vote as a lightweight count, but the canonical final result is always computed by the algorithm at close.

### Key Entities

- **Election**: id (nanoid), name, start_time, end_time, status, rules_id (e.g. `"plurality"`), rsa_pub_key (DER base64), rsa_priv_key (stored encrypted or env-only)
- **Candidate**: id (u8), election_id, name
- **RegistrationToken**: token (UUID/random, base64), election_id, used (bool), voter_pubkey (null until used)
- **AuthorizedVoter**: election_id, voter_pubkey (hex), token_used (bool) — populated when voter registers
- **UsedNonce**: election_id, h_n (SHA256 hex) — prevents double voting
- **Vote**: election_id, candidate_ids (JSON array TEXT, e.g. `[3]` or `[3,1,4,2]`), recorded_at — no voter identity
- **ElectionRules** (in-memory, loaded from TOML): deserialized rule configuration for a specific election — not stored in SQLite, re-loaded from `rules/{rules_id}.toml` on demand

---

## Success Criteria

- **SC-001**: All 5 P1 user stories pass their acceptance scenarios in integration tests.
- **SC-002**: The EC correctly prevents double-voting in 100% of test cases (nonce reuse rejection).
- **SC-003**: The EC correctly rejects invalid blind signatures in 100% of test cases.
- **SC-004**: The EC recovers full state after restart with no data loss in all test cases.
- **SC-005**: `cargo clippy --all-targets --all-features -- -D warnings` passes clean.
- **SC-006**: `cargo test` passes with coverage of all cryptographic paths (token generation, blind signing, vote verification).
- **SC-007**: The Nostr relay publish flow works end-to-end (election created → Kind 35000 visible on relay).
- **SC-008**: A plurality election with 5 votes counts correctly via `PluralityAlgorithm::count()`.
- **SC-009**: An STV election with 10 ballots (ranked) counts correctly via `StvAlgorithm::count()` and produces a valid result with the configured number of seats.
- **SC-010**: The EC rejects a ballot with too many choices for a plurality election with `BALLOT_INVALID`.
- **SC-011**: A new `.toml` rule file dropped in `rules/` is usable immediately in a new election without restarting the daemon (hot rule loading).
