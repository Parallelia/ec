//! Coverage of hybrid configuration loading (ec.toml + env vars).
//!
//! Env vars are process-global, so every test grabs a shared mutex and
//! restores a clean slate before calling `Config::load()`. Integration test
//! binaries run one at a time, so no other test file races on the
//! environment.

use std::sync::Mutex;

use secrecy::ExposeSecret;

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
];

fn with_env(vars: &[(&str, &str)], test: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap();
    for var in ALL_VARS {
        // SAFETY: all env mutation in this binary is serialized by ENV_LOCK.
        unsafe { std::env::remove_var(var) };
    }
    for (key, value) in vars {
        unsafe { std::env::set_var(key, value) };
    }
    test();
    for var in ALL_VARS {
        unsafe { std::env::remove_var(var) };
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
