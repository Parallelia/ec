use std::sync::Arc;

use anyhow::Result;
use nostr_sdk::prelude::{Client, Keys};
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use tracing_subscriber::EnvFilter;

use ec::config::Config;
use ec::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    // Load config first so LOG_LEVEL / ec.toml log_level can seed the filter.
    let config = Config::load()?;
    init_tracing(&config.log_level);

    tracing::info!(
        relay_url = %config.relay_url,
        grpc_bind = %config.grpc_bind,
        rules_dir = %config.rules_dir.display(),
        db_path = %config.db_path,
        "Configuration loaded"
    );

    if config.db_password.is_some() {
        tracing::warn!(
            "EC_DB_PASSWORD is set but database encryption is not implemented yet; the value is ignored"
        );
    }

    let options = SqliteConnectOptions::from_str(&config.db_path)?.create_if_missing(true);

    let db = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    // Run pending migrations
    sqlx::migrate!("./migrations").run(&db).await?;

    // Initialize Nostr keys and client from the configured secret key.
    let nostr_sk = config.nostr_private_key.expose_secret();
    let keys = Keys::parse(nostr_sk)?;
    let nostr_client = Client::builder().signer(keys.clone()).build();
    nostr_client.add_relay(&config.relay_url).await?;
    nostr_client.connect().await;
    let ec_nostr_keys = keys;

    let state = Arc::new(AppState {
        db,
        nostr_client,
        ec_nostr_keys,
        config,
    });

    tracing::info!("EC daemon starting");

    // Spawn the scheduler (30s tick: status transitions + counting + result publishing).
    let scheduler_handle = tokio::spawn(ec::scheduler::run(
        state.db.clone(),
        state.nostr_client.clone(),
        state.config.rules_dir.clone(),
    ));

    // Spawn the Nostr Gift Wrap listener.
    let listener_handle = tokio::spawn(ec::nostr::listener::listen(state.clone()));

    // Start the gRPC admin API server.
    let grpc_addr: std::net::SocketAddr = state.config.grpc_bind.parse()?;
    let admin_service = ec::grpc::admin::AdminService::new(
        state.db.clone(),
        state.config.rules_dir.clone(),
        state.nostr_client.clone(),
    );

    // Admin auth: when EC_ADMIN_TOKEN is set, every call must carry
    // `authorization: Bearer <token>`. Comparison happens over SHA-256
    // digests so the secret itself is not held in the interceptor closure.
    let expected_auth: Option<[u8; 32]> = state
        .config
        .admin_token
        .as_ref()
        .map(|t| Sha256::digest(format!("Bearer {}", t.expose_secret()).as_bytes()).into());
    if expected_auth.is_none() && !grpc_addr.ip().is_loopback() {
        tracing::warn!(
            grpc_bind = %state.config.grpc_bind,
            "gRPC admin API is bound to a non-loopback address WITHOUT EC_ADMIN_TOKEN — \
             anyone who can reach this port can administer elections"
        );
    }
    let auth_interceptor =
        move |req: tonic::Request<()>| -> Result<tonic::Request<()>, tonic::Status> {
            if let Some(expected) = expected_auth {
                let provided = req
                    .metadata()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let provided_hash: [u8; 32] = Sha256::digest(provided.as_bytes()).into();
                if provided_hash != expected {
                    return Err(tonic::Status::unauthenticated(
                        "invalid or missing admin token",
                    ));
                }
            }
            Ok(req)
        };

    let grpc_handle = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(
                ec::grpc::proto::admin_server::AdminServer::with_interceptor(
                    admin_service,
                    auth_interceptor,
                ),
            )
            .serve(grpc_addr),
    );

    tracing::info!(grpc_bind = %state.config.grpc_bind, "EC daemon running");

    // Wait for any task to finish (they run forever under normal operation).
    tokio::select! {
        res = scheduler_handle => {
            match res {
                Ok(()) => {
                    tracing::error!("Scheduler exited unexpectedly");
                    anyhow::bail!("Scheduler exited unexpectedly")
                }
                Err(join_err) => Err(join_err.into()),
            }
        }
        res = listener_handle => {
            match res {
                Ok(Ok(())) => {
                    tracing::error!("Nostr listener exited unexpectedly");
                    anyhow::bail!("Nostr listener exited unexpectedly")
                }
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(join_err.into()),
            }
        }
        res = grpc_handle => {
            match res {
                Ok(Ok(())) => {
                    tracing::error!("gRPC server exited unexpectedly");
                    anyhow::bail!("gRPC server exited unexpectedly")
                }
                Ok(Err(e)) => Err(e.into()),
                Err(join_err) => Err(join_err.into()),
            }
        }
    }
}

/// RUST_LOG takes precedence; otherwise fall back to the configured level
/// (LOG_LEVEL env var or `log_level` in ec.toml).
fn init_tracing(log_level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
