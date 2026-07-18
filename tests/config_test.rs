//! Coverage of hybrid configuration loading (ec.toml + env vars).
//!
//! Env vars and the working directory are process-global, so every test grabs
//! a shared mutex and runs in a fresh temp directory that holds only a copy of
//! the repository `ec.toml`. This isolates `Config::load()` from the
//! developer's local `.env` (which `dotenvy` would otherwise inject, and which
//! lives in the crate root) and from any real env vars. Integration test
//! binaries run one at a time, so no other test file races on the environment.

use std::fs;
use std::sync::Mutex;

use secrecy::ExposeSecret;
use tempfile::TempDir;

use ec::config::Config;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const ALL_VARS: &[&str] = &[
    "RELAY_URL",
    "GRPC_BIND",
    "RULES_DIR",
    "LOG_LEVEL",
    "DATABASE_URL",
    "NOSTR_PRIVATE_KEY",
    "EC_DB_PASSWORD",
    "EC_ADMIN_TOKEN",
    "TLS_CERT",
    "TLS_KEY",
];

fn with_env(vars: &[(&str, &str)], test: impl FnOnce()) {
    with_env_and_toml(vars, "", test);
}

/// Like [`with_env`], but appends `toml_append` to the copied `ec.toml` so a
/// test can exercise keys the repository config does not set.
fn with_env_and_toml(vars: &[(&str, &str)], toml_append: &str, test: impl FnOnce()) {
    // Recover from a poisoned lock so a failure in one test does not mask the
    // real cause by surfacing as `PoisonError` in every test that follows.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Copy the repository ec.toml into an isolated temp dir that has no `.env`
    // in any ancestor, so `Config::load()` sees exactly the state we set up.
    let mut ec_toml = fs::read_to_string("ec.toml").expect("repository ec.toml must exist");
    if !toml_append.is_empty() {
        ec_toml.push('\n');
        ec_toml.push_str(toml_append);
        ec_toml.push('\n');
    }
    let tmp = TempDir::new().expect("create temp dir");
    fs::write(tmp.path().join("ec.toml"), ec_toml).expect("write temp ec.toml");

    let original_dir = std::env::current_dir().expect("read cwd");
    std::env::set_current_dir(tmp.path()).expect("enter temp dir");

    for var in ALL_VARS {
        // SAFETY: all env mutation in this binary is serialized by ENV_LOCK.
        unsafe { std::env::remove_var(var) };
    }
    for (key, value) in vars {
        unsafe { std::env::set_var(key, value) };
    }

    // Catch panics so the working directory and env are always restored, even
    // when an assertion fails; then re-raise for the test harness to report.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test));

    for var in ALL_VARS {
        unsafe { std::env::remove_var(var) };
    }
    std::env::set_current_dir(&original_dir).expect("restore cwd");

    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn load_fails_without_nostr_private_key() {
    with_env(&[], || {
        let err = Config::load().expect_err("missing NOSTR_PRIVATE_KEY must fail");
        assert!(err.to_string().contains("NOSTR_PRIVATE_KEY"));
    });
}

#[test]
fn load_uses_ec_toml_values_when_env_absent() {
    with_env(&[("NOSTR_PRIVATE_KEY", "nsec-test")], || {
        let config = Config::load().expect("load must succeed");
        // Values come from the repository's ec.toml.
        assert_eq!(config.relay_url, "wss://relay.mostro.network");
        assert_eq!(config.grpc_bind, "127.0.0.1:50051");
        assert_eq!(config.rules_dir, std::path::PathBuf::from("./rules"));
        assert_eq!(config.log_level, "info");
        // Plain file path gets the sqlite: scheme prepended.
        assert_eq!(config.db_path, "sqlite:./ec.db");
        assert_eq!(config.nostr_private_key.expose_secret(), "nsec-test");
        assert!(config.db_password.is_none());
        assert!(config.admin_token.is_none());
    });
}

#[test]
fn env_vars_override_file_config() {
    with_env(
        &[
            ("NOSTR_PRIVATE_KEY", "nsec-test"),
            ("RELAY_URL", "ws://127.0.0.1:7777"),
            ("GRPC_BIND", "0.0.0.0:9999"),
            ("RULES_DIR", "/custom/rules"),
            ("LOG_LEVEL", "trace"),
            ("DATABASE_URL", "sqlite::memory:"),
            ("EC_DB_PASSWORD", "hunter2"),
            ("EC_ADMIN_TOKEN", "admin-secret"),
        ],
        || {
            let config = Config::load().expect("load must succeed");
            assert_eq!(config.relay_url, "ws://127.0.0.1:7777");
            assert_eq!(config.grpc_bind, "0.0.0.0:9999");
            assert_eq!(config.rules_dir, std::path::PathBuf::from("/custom/rules"));
            assert_eq!(config.log_level, "trace");
            // Already sqlite-prefixed → left untouched.
            assert_eq!(config.db_path, "sqlite::memory:");
            assert_eq!(
                config.db_password.as_ref().unwrap().expose_secret(),
                "hunter2"
            );
            assert_eq!(
                config.admin_token.as_ref().unwrap().expose_secret(),
                "admin-secret"
            );
        },
    );
}

#[test]
fn database_url_with_scheme_is_kept_verbatim() {
    with_env(
        &[
            ("NOSTR_PRIVATE_KEY", "nsec-test"),
            ("DATABASE_URL", "postgres://db.example/ec"),
        ],
        || {
            let config = Config::load().expect("load must succeed");
            assert_eq!(config.db_path, "postgres://db.example/ec");
        },
    );
}

#[test]
fn plain_database_path_gets_sqlite_scheme() {
    with_env(
        &[
            ("NOSTR_PRIVATE_KEY", "nsec-test"),
            ("DATABASE_URL", "/var/lib/ec/ec.db"),
        ],
        || {
            let config = Config::load().expect("load must succeed");
            assert_eq!(config.db_path, "sqlite:/var/lib/ec/ec.db");
        },
    );
}

#[test]
fn empty_admin_token_is_treated_as_unset() {
    with_env(
        &[("NOSTR_PRIVATE_KEY", "nsec-test"), ("EC_ADMIN_TOKEN", "")],
        || {
            let config = Config::load().expect("load must succeed");
            assert!(config.admin_token.is_none());
        },
    );
}

#[test]
fn tls_is_disabled_by_default() {
    with_env(&[("NOSTR_PRIVATE_KEY", "nsec-test")], || {
        let config = Config::load().expect("load must succeed");
        assert!(config.grpc_tls.is_none());
    });
}

#[test]
fn tls_paths_load_from_env() {
    with_env(
        &[
            ("NOSTR_PRIVATE_KEY", "nsec-test"),
            ("TLS_CERT", "/etc/ec/cert.pem"),
            ("TLS_KEY", "/etc/ec/key.pem"),
        ],
        || {
            let config = Config::load().expect("load must succeed");
            let tls = config.grpc_tls.expect("tls must be configured");
            assert_eq!(tls.cert, std::path::PathBuf::from("/etc/ec/cert.pem"));
            assert_eq!(tls.key, std::path::PathBuf::from("/etc/ec/key.pem"));
        },
    );
}

#[test]
fn tls_paths_load_from_ec_toml() {
    with_env_and_toml(
        &[("NOSTR_PRIVATE_KEY", "nsec-test")],
        "tls_cert = \"./cert.pem\"\ntls_key = \"./key.pem\"",
        || {
            let config = Config::load().expect("load must succeed");
            let tls = config.grpc_tls.expect("tls must be configured");
            assert_eq!(tls.cert, std::path::PathBuf::from("./cert.pem"));
            assert_eq!(tls.key, std::path::PathBuf::from("./key.pem"));
        },
    );
}

#[test]
fn tls_env_overrides_ec_toml() {
    with_env_and_toml(
        &[
            ("NOSTR_PRIVATE_KEY", "nsec-test"),
            ("TLS_CERT", "/env/cert.pem"),
            ("TLS_KEY", "/env/key.pem"),
        ],
        "tls_cert = \"./toml-cert.pem\"\ntls_key = \"./toml-key.pem\"",
        || {
            let config = Config::load().expect("load must succeed");
            let tls = config.grpc_tls.expect("tls must be configured");
            assert_eq!(tls.cert, std::path::PathBuf::from("/env/cert.pem"));
            assert_eq!(tls.key, std::path::PathBuf::from("/env/key.pem"));
        },
    );
}

#[test]
fn tls_cert_without_key_fails() {
    with_env(
        &[
            ("NOSTR_PRIVATE_KEY", "nsec-test"),
            ("TLS_CERT", "/etc/ec/cert.pem"),
        ],
        || {
            let err = Config::load().expect_err("cert without key must fail");
            assert!(err.to_string().contains("tls_key"), "got: {err}");
        },
    );
}

#[test]
fn tls_key_without_cert_fails() {
    with_env(
        &[
            ("NOSTR_PRIVATE_KEY", "nsec-test"),
            ("TLS_KEY", "/etc/ec/key.pem"),
        ],
        || {
            let err = Config::load().expect_err("key without cert must fail");
            assert!(err.to_string().contains("tls_cert"), "got: {err}");
        },
    );
}

#[test]
fn empty_tls_env_vars_are_treated_as_unset() {
    with_env(
        &[
            ("NOSTR_PRIVATE_KEY", "nsec-test"),
            ("TLS_CERT", ""),
            ("TLS_KEY", ""),
        ],
        || {
            let config = Config::load().expect("load must succeed");
            assert!(config.grpc_tls.is_none());
        },
    );
}
