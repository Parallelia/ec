use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use secrecy::SecretString;
use serde::Deserialize;

/// PEM certificate/key pair enabling TLS on the gRPC admin API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Hybrid configuration loaded from `ec.toml` (non-secrets) plus env vars (secrets & overrides).
#[derive(Debug, Clone)]
pub struct Config {
    // --- From ec.toml (non-secret) ---
    pub relay_url: String,
    pub grpc_bind: String,
    pub rules_dir: PathBuf,
    pub log_level: String,
    pub db_path: String,
    /// TLS for the gRPC admin API; `None` serves plaintext (loopback only!).
    pub grpc_tls: Option<TlsPaths>,

    // --- From env vars (secrets) ---
    pub nostr_private_key: SecretString,
    pub db_password: Option<SecretString>,
    /// Bearer token required on every gRPC admin call when set.
    pub admin_token: Option<SecretString>,
}

#[derive(Debug, Clone, Deserialize)]
struct FileConfig {
    relay_url: String,
    grpc_bind: String,
    rules_dir: String,
    log_level: String,
    db_path: String,
    tls_cert: Option<String>,
    tls_key: Option<String>,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            relay_url: "wss://relay.mostro.network".to_string(),
            grpc_bind: "127.0.0.1:50051".to_string(),
            rules_dir: "./rules".to_string(),
            log_level: "info".to_string(),
            db_path: "./ec.db".to_string(),
            tls_cert: None,
            tls_key: None,
        }
    }
}

/// Combine the optional cert/key settings into a validated pair.
///
/// Setting only one of the two is a configuration mistake that would silently
/// serve plaintext, so it is rejected rather than ignored.
fn resolve_tls(cert: Option<String>, key: Option<String>) -> Result<Option<TlsPaths>> {
    match (cert, key) {
        (Some(cert), Some(key)) => Ok(Some(TlsPaths {
            cert: PathBuf::from(cert),
            key: PathBuf::from(key),
        })),
        (None, None) => Ok(None),
        (Some(_), None) => bail!("tls_cert is set but tls_key is not — both are required for TLS"),
        (None, Some(_)) => bail!("tls_key is set but tls_cert is not — both are required for TLS"),
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        // 1. Load .env if present (development convenience)
        let _ = dotenvy::dotenv();

        // 2. Load ec.toml if present, else use defaults
        let file_config = Self::load_toml("ec.toml").unwrap_or_default();

        // 3. Env vars override file config
        let relay_url =
            std::env::var("RELAY_URL").unwrap_or_else(|_| file_config.relay_url.clone());
        let grpc_bind =
            std::env::var("GRPC_BIND").unwrap_or_else(|_| file_config.grpc_bind.clone());
        let rules_dir = PathBuf::from(
            std::env::var("RULES_DIR").unwrap_or_else(|_| file_config.rules_dir.clone()),
        );
        let log_level =
            std::env::var("LOG_LEVEL").unwrap_or_else(|_| file_config.log_level.clone());
        let db_path = std::env::var("DATABASE_URL").unwrap_or_else(|_| file_config.db_path.clone());
        let db_path = if db_path.contains("://") || db_path.starts_with("sqlite:") {
            db_path
        } else {
            format!("sqlite:{db_path}")
        };

        let tls_cert = std::env::var("TLS_CERT")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| file_config.tls_cert.clone());
        let tls_key = std::env::var("TLS_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| file_config.tls_key.clone());
        let grpc_tls = resolve_tls(tls_cert, tls_key)?;

        let nostr_private_key = SecretString::new(
            std::env::var("NOSTR_PRIVATE_KEY")
                .context("NOSTR_PRIVATE_KEY env var is required")?
                .into_boxed_str(),
        );
        let db_password = std::env::var("EC_DB_PASSWORD")
            .ok()
            .map(|s| SecretString::new(s.into_boxed_str()));
        let admin_token = std::env::var("EC_ADMIN_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| SecretString::new(s.into_boxed_str()));

        Ok(Self {
            relay_url,
            grpc_bind,
            rules_dir,
            log_level,
            db_path,
            grpc_tls,
            nostr_private_key,
            db_password,
            admin_token,
        })
    }

    fn load_toml(path: &str) -> Result<FileConfig> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path))?;
        let cfg: FileConfig =
            toml::from_str(&content).with_context(|| format!("failed to parse {}", path))?;
        Ok(cfg)
    }
}
