//! TLS identity loading for the gRPC admin API.

use anyhow::{Context, Result};
use tonic::transport::Identity;

use crate::config::TlsPaths;

/// Read the configured PEM certificate/key pair into a server [`Identity`].
///
/// The PEM contents are validated by tonic when the server starts, so this
/// only has to get the bytes off disk with actionable errors.
pub fn load_identity(paths: &TlsPaths) -> Result<Identity> {
    let cert = std::fs::read(&paths.cert)
        .with_context(|| format!("reading TLS certificate '{}'", paths.cert.display()))?;
    let key = std::fs::read(&paths.key)
        .with_context(|| format!("reading TLS private key '{}'", paths.key.display()))?;
    Ok(Identity::from_pem(cert, key))
}
