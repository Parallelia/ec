use nostr_sdk::prelude::Client;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tonic::{Request, Response, Status};

use crate::grpc::proto::admin_server::Admin;
use crate::grpc::proto::{
    AddCandidateRequest, AddElectionRequest, CandidateResponse, ElectionIdRequest,
    ElectionListResponse, ElectionResponse, Empty, GenerateTokensRequest, StatusResponse,
    TokenInfo, TokenListResponse, TokensResponse,
};
use crate::types::{Candidate, Election};
use crate::{crypto, db, nostr, rules};

pub struct AdminService {
    pool: SqlitePool,
    rules_dir: std::path::PathBuf,
    nostr_client: Client,
}

impl AdminService {
    pub fn new(pool: SqlitePool, rules_dir: std::path::PathBuf, nostr_client: Client) -> Self {
        Self {
            pool,
            rules_dir,
            nostr_client,
        }
    }
}

fn election_to_response(e: &Election) -> ElectionResponse {
    ElectionResponse {
        id: e.id.clone(),
        name: e.name.clone(),
        start_time: e.start_time,
        end_time: e.end_time,
        status: e.status.clone(),
        rules_id: e.rules_id.clone(),
        rsa_pub_key: e.rsa_pub_key.clone(),
        created_at: e.created_at,
    }
}

#[tonic::async_trait]
impl Admin for AdminService {
    async fn add_election(
        &self,
        request: Request<AddElectionRequest>,
    ) -> Result<Response<ElectionResponse>, Status> {
        let req = request.into_inner();

        // Validate time constraints
        let now = chrono::Utc::now().timestamp();
        if req.start_time < now {
            return Err(Status::invalid_argument("start_time must be in the future"));
        }
        if req.end_time <= req.start_time {
            return Err(Status::invalid_argument(
                "end_time must be greater than start_time",
            ));
        }

        // Sanitize rules_id to prevent path traversal
        if req.rules_id.is_empty()
            || req.rules_id.contains('/')
            || req.rules_id.contains('\\')
            || req.rules_id.contains("..")
        {
            return Err(Status::invalid_argument("Invalid rules_id"));
        }

        // Validate rules exist before creating election
        rules::load_rules(&req.rules_id, &self.rules_dir)
            .map_err(|_| Status::invalid_argument("Invalid rules_id"))?;

        // Generate RSA keypair for this election
        let (pk_b64, sk_b64) =
            crypto::generate_keypair().map_err(|e| Status::internal(e.to_string()))?;

        let election_id = nanoid::nanoid!();
        let now = chrono::Utc::now().timestamp();

        let election = Election {
            id: election_id,
            name: req.name,
            start_time: req.start_time,
            end_time: req.end_time,
            status: "open".to_string(),
            rules_id: req.rules_id,
            rsa_pub_key: pk_b64,
            created_at: now,
            results_published: 0,
        };

        // Re-check: keypair generation may have taken enough time for start_time to lapse.
        if election.start_time < chrono::Utc::now().timestamp() {
            return Err(Status::invalid_argument("start_time must be in the future"));
        }

        let sk_secret = SecretString::new(sk_b64.into_boxed_str());
        db::create_election(&self.pool, &election, &sk_secret)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        tracing::info!(election_id = %election.id, "Election created");

        // Publish Kind 35000 election announcement to Nostr (empty candidates at creation).
        if let Err(e) =
            nostr::publisher::publish_election_event(&self.nostr_client, &election, &[]).await
        {
            tracing::warn!(
                election_id = %election.id,
                error = %e,
                "Failed to publish election announcement to Nostr"
            );
        }

        Ok(Response::new(election_to_response(&election)))
    }

    async fn add_candidate(
        &self,
        request: Request<AddCandidateRequest>,
    ) -> Result<Response<CandidateResponse>, Status> {
        let req = request.into_inner();

        // Validate candidate ID fits in u8 range
        if req.id > 255 {
            return Err(Status::invalid_argument(
                "Candidate ID must be between 0 and 255",
            ));
        }

        // Atomically insert candidate only if election is open
        let candidate = Candidate {
            id: req.id as u8,
            election_id: req.election_id,
            name: req.name,
        };

        let rows = db::add_candidate_if_open(&self.pool, &candidate)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        if rows == 0 {
            return Err(Status::failed_precondition(
                "Election not found or not open for candidate registration",
            ));
        }

        tracing::info!(
            election_id = %candidate.election_id,
            candidate_id = candidate.id,
            "Candidate added"
        );

        // Re-publish Kind 35000 with updated candidate list (addressable event replaces previous).
        let election = db::get_election(&self.pool, &candidate.election_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        if let Some(election) = election {
            let candidates = db::get_candidates_for_election(&self.pool, &candidate.election_id)
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
            if let Err(e) =
                nostr::publisher::publish_election_event(&self.nostr_client, &election, &candidates)
                    .await
            {
                tracing::warn!(
                    election_id = %election.id,
                    error = %e,
                    "Failed to re-publish election announcement to Nostr"
                );
            }
        }

        Ok(Response::new(CandidateResponse {
            id: candidate.id as u32,
            election_id: candidate.election_id,
            name: candidate.name,
        }))
    }

    async fn cancel_election(
        &self,
        request: Request<ElectionIdRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        let election_id = &request.into_inner().election_id;

        let rows = db::cancel_election(&self.pool, election_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        if rows == 0 {
            return Err(Status::failed_precondition(
                "Election not found or already finished/cancelled",
            ));
        }

        tracing::info!(election_id = %election_id, "Election cancelled");

        // Re-publish the announcement so relay clients see the cancelled status.
        match db::get_election(&self.pool, election_id).await {
            Ok(Some(election)) => {
                let candidates = db::get_candidates_for_election(&self.pool, election_id)
                    .await
                    .unwrap_or_default();
                if let Err(e) = nostr::publisher::publish_election_event(
                    &self.nostr_client,
                    &election,
                    &candidates,
                )
                .await
                {
                    tracing::warn!(
                        election_id = %election_id,
                        error = %e,
                        "Failed to re-publish cancelled election announcement"
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(election_id = %election_id, error = %e, "Failed to reload election after cancel");
            }
        }

        Ok(Response::new(StatusResponse {
            success: true,
            message: "Election cancelled".to_string(),
        }))
    }

    async fn get_election(
        &self,
        request: Request<ElectionIdRequest>,
    ) -> Result<Response<ElectionResponse>, Status> {
        let election_id = &request.into_inner().election_id;

        let election = db::get_election(&self.pool, election_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("Election not found"))?;

        Ok(Response::new(election_to_response(&election)))
    }

    async fn list_elections(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ElectionListResponse>, Status> {
        let elections = db::list_elections(&self.pool)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let responses = elections.iter().map(election_to_response).collect();

        Ok(Response::new(ElectionListResponse {
            elections: responses,
        }))
    }

    async fn generate_registration_tokens(
        &self,
        request: Request<GenerateTokensRequest>,
    ) -> Result<Response<TokensResponse>, Status> {
        let req = request.into_inner();

        const MAX_TOKENS_PER_REQUEST: u32 = 10_000;
        if req.count == 0 || req.count > MAX_TOKENS_PER_REQUEST {
            return Err(Status::invalid_argument(format!(
                "count must be between 1 and {MAX_TOKENS_PER_REQUEST}"
            )));
        }

        // Validate election exists and can still enroll voters
        let election = db::get_election(&self.pool, &req.election_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("Election not found"))?;

        if election.status != "open" && election.status != "in_progress" {
            return Err(Status::failed_precondition(
                "Election is not accepting registrations (finished or cancelled)",
            ));
        }

        // Generate random tokens
        let tokens: Vec<String> = (0..req.count)
            .map(|_| {
                let mut bytes = [0u8; 32];
                rand::RngExt::fill(&mut rand::rng(), &mut bytes);
                base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
            })
            .collect();

        // Insert in a transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let inserted = db::insert_registration_tokens(&mut tx, &req.election_id, &tokens)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        if inserted != tokens.len() as u64 {
            return Err(Status::internal("Failed to insert all registration tokens"));
        }

        tx.commit()
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        tracing::info!(
            election_id = %req.election_id,
            count = tokens.len(),
            "Registration tokens generated"
        );

        Ok(Response::new(TokensResponse { tokens }))
    }

    async fn list_registration_tokens(
        &self,
        request: Request<ElectionIdRequest>,
    ) -> Result<Response<TokenListResponse>, Status> {
        let election_id = &request.into_inner().election_id;

        // Validate election exists
        db::get_election(&self.pool, election_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("Election not found"))?;

        let tokens = db::list_registration_tokens(&self.pool, election_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let token_infos = tokens
            .iter()
            .map(|t| {
                // Show truncated hash of token for display — never expose raw token
                let hash = Sha256::digest(t.token.as_bytes());
                let token_id = hex::encode(&hash[..8]);
                TokenInfo {
                    token_id,
                    used: t.used != 0,
                }
            })
            .collect();

        Ok(Response::new(TokenListResponse {
            tokens: token_infos,
        }))
    }
}
