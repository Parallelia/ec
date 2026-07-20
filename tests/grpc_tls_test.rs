//! Coverage of gRPC admin API TLS: identity loading from PEM files and a real
//! handshake against a TLS-configured server (issue #18).

mod common;

use nostr_sdk::prelude::{Client, Keys};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tempfile::TempDir;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Server, ServerTlsConfig};

use ec::config::TlsPaths;
use ec::grpc::admin::AdminService;
use ec::grpc::proto::Empty;
use ec::grpc::proto::admin_client::AdminClient;
use ec::grpc::proto::admin_server::AdminServer;
use ec::grpc::tls::load_identity;

fn write_pair(dir: &TempDir, cert_pem: &str, key_pem: &str) -> TlsPaths {
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    std::fs::write(&cert, cert_pem).unwrap();
    std::fs::write(&key, key_pem).unwrap();
    TlsPaths { cert, key }
}

fn self_signed() -> (String, String) {
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .expect("generate self-signed certificate");
    (cert.pem(), key_pair.serialize_pem())
}

#[test]
fn load_identity_reads_pem_pair() {
    let (cert_pem, key_pem) = self_signed();
    let dir = TempDir::new().unwrap();
    let paths = write_pair(&dir, &cert_pem, &key_pem);

    assert!(load_identity(&paths).is_ok());
}

#[test]
fn load_identity_missing_cert_names_the_path() {
    let dir = TempDir::new().unwrap();
    let paths = TlsPaths {
        cert: dir.path().join("nope.pem"),
        key: dir.path().join("also-nope.pem"),
    };

    let err = load_identity(&paths).expect_err("missing cert must fail");
    assert!(err.to_string().contains("nope.pem"), "got: {err}");
}

#[test]
fn load_identity_missing_key_names_the_path() {
    let (cert_pem, _) = self_signed();
    let dir = TempDir::new().unwrap();
    let cert = dir.path().join("cert.pem");
    std::fs::write(&cert, cert_pem).unwrap();
    let paths = TlsPaths {
        cert,
        key: dir.path().join("missing-key.pem"),
    };

    let err = load_identity(&paths).expect_err("missing key must fail");
    assert!(err.to_string().contains("missing-key.pem"), "got: {err}");
}

async fn setup_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

/// Nostr client with no relays — announcements are best-effort.
fn offline_client() -> Client {
    Client::builder().signer(Keys::generate()).build()
}

/// End-to-end: a TLS-configured server completes a handshake with a TLS
/// client, and a plaintext client cannot talk to it.
#[tokio::test]
async fn tls_server_accepts_tls_client_and_rejects_plaintext() {
    common::init_tracing();
    let (cert_pem, key_pem) = self_signed();
    let dir = TempDir::new().unwrap();
    let paths = write_pair(&dir, &cert_pem, &key_pem);
    let identity = load_identity(&paths).unwrap();

    let pool = setup_pool().await;
    let service = AdminService::new(
        pool.clone(),
        std::path::PathBuf::from("rules"),
        offline_client(),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(identity))
            .unwrap()
            .add_service(AdminServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );

    // TLS client trusting the self-signed cert: the RPC must succeed.
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(&cert_pem))
        .domain_name("localhost");
    let channel = Channel::from_shared(format!("https://127.0.0.1:{}", addr.port()))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .expect("TLS handshake must succeed");
    let mut client = AdminClient::new(channel);
    let elections = client
        .list_elections(Empty {})
        .await
        .expect("RPC over TLS must succeed")
        .into_inner();
    assert!(elections.elections.is_empty());

    // Plaintext client against the TLS server: the call must fail.
    let plaintext = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let channel = Channel::from_shared(format!("http://127.0.0.1:{}", addr.port()))
            .unwrap()
            .connect()
            .await?;
        AdminClient::new(channel).list_elections(Empty {}).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await
    .expect("plaintext attempt must not hang");
    assert!(
        plaintext.is_err(),
        "plaintext client must not reach a TLS-only server"
    );

    server.abort();
}
