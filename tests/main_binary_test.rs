//! Smoke tests for the `ec` daemon binary. Each scenario drives `main()` to a
//! controlled error exit so the process terminates on its own (startup paths
//! are executed for real: config, DB migrations, Nostr client, gRPC setup).

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use nostr_sdk::prelude::Keys;

/// Wait for the child to exit on its own; kill it if it is still alive after
/// the deadline (which fails the assertion in the caller).
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

fn base_command(dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ec"));
    // Run from an empty temp dir: no ec.toml, no .env, defaults apply.
    cmd.current_dir(dir)
        .env_remove("NOSTR_PRIVATE_KEY")
        .env_remove("EC_ADMIN_TOKEN")
        .env_remove("EC_DB_PASSWORD")
        .env_remove("RUST_LOG")
        .env_remove("GRPC_BIND")
        .env_remove("RULES_DIR")
        .env("DATABASE_URL", dir.join("ec.db").display().to_string())
        // Unreachable relay: add_relay/connect don't block startup.
        .env("RELAY_URL", "ws://127.0.0.1:1")
        .env("LOG_LEVEL", "info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

fn ec_secret_key_hex() -> String {
    Keys::generate().secret_key().to_secret_hex()
}

#[test]
fn exits_with_error_when_nostr_key_missing() {
    let dir = tempfile::tempdir().unwrap();
    // A malformed ec.toml must not crash startup — it falls back to defaults
    // (and the daemon then fails on the missing NOSTR_PRIVATE_KEY).
    std::fs::write(dir.path().join("ec.toml"), "this is not [valid toml").unwrap();
    let mut child = base_command(dir.path()).spawn().expect("spawn ec");

    let status = wait_with_timeout(&mut child, Duration::from_secs(20))
        .expect("daemon must exit when NOSTR_PRIVATE_KEY is missing");
    assert!(!status.success());
}

#[test]
fn exits_with_error_on_unparseable_grpc_bind() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = base_command(dir.path())
        .env("NOSTR_PRIVATE_KEY", ec_secret_key_hex())
        .env("GRPC_BIND", "not-a-socket-address")
        .spawn()
        .expect("spawn ec");

    let status = wait_with_timeout(&mut child, Duration::from_secs(20))
        .expect("daemon must exit on invalid GRPC_BIND");
    assert!(!status.success());
}

#[test]
fn full_startup_then_grpc_bind_conflict_with_admin_token() {
    let dir = tempfile::tempdir().unwrap();
    // Occupy a loopback port so the gRPC server fails to bind after the rest
    // of the daemon (config, DB, migrations, Nostr, scheduler) started fine.
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = blocker.local_addr().unwrap();

    let mut child = base_command(dir.path())
        .env("NOSTR_PRIVATE_KEY", ec_secret_key_hex())
        .env("EC_ADMIN_TOKEN", "test-admin-token")
        .env("EC_DB_PASSWORD", "ignored-for-now")
        .env("GRPC_BIND", addr.to_string())
        .spawn()
        .expect("spawn ec");

    let status = wait_with_timeout(&mut child, Duration::from_secs(20))
        .expect("daemon must exit when the gRPC port is taken");
    assert!(!status.success());
    drop(blocker);
}

#[test]
fn full_startup_warns_on_public_bind_without_token() {
    let dir = tempfile::tempdir().unwrap();
    // Non-loopback bind without EC_ADMIN_TOKEN triggers the security warning
    // before the (occupied) port makes the daemon exit.
    let blocker = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    let port = blocker.local_addr().unwrap().port();

    let mut child = base_command(dir.path())
        .env("NOSTR_PRIVATE_KEY", ec_secret_key_hex())
        .env("GRPC_BIND", format!("0.0.0.0:{port}"))
        .spawn()
        .expect("spawn ec");

    let status = wait_with_timeout(&mut child, Duration::from_secs(20))
        .expect("daemon must exit when the gRPC port is taken");
    assert!(!status.success());
    drop(blocker);
}
