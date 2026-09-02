//! OIDC portal tests share one PostgreSQL database named `oidc_test` (default
//! `postgres://test:test@127.0.0.1:15433/oidc_test`, override with
//! `OIDC_TEST_DATABASE_URL`).  The database is created on first use and all
//! five legacy/portal tables are dropped before each test so the schema
//! initializer rebuilds them from `SCHEMA_SQL` in their current shape.
//!
//! Tests skip (with a message) when no reachable PostgreSQL is configured;
//! CI without the test database stays green the same way
//! `tests/postgres_roundtrip.rs` does.

use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use tokio_postgres::{Config, NoTls};

pub fn database_url() -> Option<String> {
    std::env::var("OIDC_TEST_DATABASE_URL")
        .ok()
        .or_else(|| Some("postgres://test:test@127.0.0.1:15433/oidc_test".to_string()))
}

/// Serialize tests in this binary that share the database.
pub fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

async fn connect(database_url: &str) -> Result<tokio_postgres::Client, tokio_postgres::Error> {
    let mut config = Config::from_str(database_url)?;
    if config.get_password().is_none() {
        if let Ok(password) = std::env::var("PGPASSWORD") {
            config.password(password);
        }
    }
    let (client, connection) = config.connect(NoTls).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// Ensure the test database exists; returns `false` when PostgreSQL is
/// unreachable (callers skip).
pub async fn ensure_database(database_url: &str) -> bool {
    let (admin_url, db_name) = split_admin(database_url);
    let Ok(client) = connect(&admin_url).await else {
        eprintln!(
            "skipping portal oidc test: PostgreSQL with OIDC_TEST_DATABASE_URL is unavailable"
        );
        return false;
    };
    let exists = client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
            &[&db_name],
        )
        .await
        .map(|row| row.get::<_, bool>(0))
        .unwrap_or(false);
    if !exists {
        let escaped = quote_ident(&db_name);
        let result = client
            .batch_execute(&format!("CREATE DATABASE {escaped}"))
            .await;
        if let Err(error) = result {
            // Concurrent test binaries may race on CREATE DATABASE; treat a
            // duplicate-database error as success.
            eprintln!("create database warning: {error}");
        }
    }
    true
}

/// Drop every table the OIDC feature or the superseded engine created so the
/// schema initializer rebuilds them fresh.
pub async fn reset_portal_tables(database_url: &str) {
    let client = connect(database_url)
        .await
        .expect("oidc test db must connect");
    client
        .batch_execute(
            "DROP TABLE IF EXISTS portal_sessions, portal_user_downstreams, \
             portal_identities, portal_users, oauth_login_attempts CASCADE",
        )
        .await
        .expect("dropping legacy portal tables must succeed");
}

fn split_admin(database_url: &str) -> (String, String) {
    let db_name = database_url
        .split('/')
        .next_back()
        .unwrap_or("oidc_test")
        .to_string();
    let mut parts: Vec<&str> = database_url.splitn(4, '/').collect();
    if parts.len() == 4 {
        parts[3] = "postgres";
    }
    (parts.join("/"), db_name)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
