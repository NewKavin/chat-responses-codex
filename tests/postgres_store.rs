//! Postgres write-path regression tests.
//!
//! The Postgres backend (`DATABASE_URL`) is the only production persistence
//! path that runs real SQL; the file backend hides SQL bugs. These tests
//! exercise `persist_config` end-to-end against a real PostgreSQL and assert
//! that the generated INSERT statements can never drift from their bound
//! parameter lists (regression guard for the f44dfc7 class of bug).
//!
//! Set `TEST_DATABASE_URL` (or the legacy `PG_TEST_DATABASE_URL`) to enable
//! the database-backed tests; without it they skip.

use chat_responses_codex::keys::generate_downstream_key;
use chat_responses_codex::routing::UpstreamProtocol;
use chat_responses_codex::state::{
    insert_statement, AppConfig, AppState, DownstreamConfig, NonstandardFieldPolicy, UpstreamConfig,
};
use std::env;
use std::str::FromStr;
use std::sync::OnceLock;
use tokio::sync::Mutex;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn test_database_url() -> Option<String> {
    env::var("TEST_DATABASE_URL")
        .ok()
        .or_else(|| env::var("PG_TEST_DATABASE_URL").ok())
}

async fn postgres_client(database_url: &str) -> tokio_postgres::Client {
    let mut config = tokio_postgres::Config::from_str(database_url).unwrap();
    if config.get_password().is_none() {
        if let Ok(password) = env::var("PGPASSWORD") {
            config.password(password);
        }
    }
    let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move {
        connection
            .await
            .expect("postgres test connection should remain healthy");
    });
    client
}

async fn reset_test_database(database_url: &str) {
    postgres_client(database_url)
        .await
        .batch_execute(
            "TRUNCATE TABLE response_history, usage_logs, dialect_profiles, \
             downstream_ip_allowlist, downstream_model_allowlist, downstreams, \
             upstream_supported_models, upstreams, \
             global_context_profiles, app_announcements, runtime_settings RESTART IDENTITY",
        )
        .await
        .unwrap();
}

#[test]
fn insert_statement_generates_one_placeholder_per_column() {
    let sql = insert_statement(
        "upstreams",
        &["id", "name", "base_url"],
        "ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
    );
    assert!(sql.contains("INSERT INTO upstreams ("));
    assert!(sql.contains("id, name, base_url"));
    assert!(sql.contains("VALUES (\n    $1, $2, $3\n)"));
    assert!(sql.ends_with("ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name"));

    // Column count == highest placeholder index == placeholder count.
    let column_count = sql.split(") VALUES").next().unwrap().split(',').count();
    let values_section = sql
        .split("VALUES")
        .nth(1)
        .unwrap()
        .split("ON CONFLICT")
        .next()
        .unwrap();
    let placeholders: Vec<usize> = values_section
        .split('$')
        .skip(1)
        .map(|part| {
            part.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<usize>()
                .unwrap()
        })
        .collect();
    assert_eq!(placeholders, vec![1, 2, 3]);
    assert_eq!(column_count, *placeholders.last().unwrap());
}

/// Guards against a repeat of the f44dfc7 regression class: every
/// hand-written `INSERT ... VALUES` statement in postgres.rs must have a
/// column list whose length equals the number of placeholders (plus any
/// inline string literals). Statements with generated placeholders
/// (`{placeholders}`) or no placeholders at all are skipped.
#[test]
fn hand_written_insert_statements_have_balanced_columns_and_placeholders() {
    let source = include_str!("../src/state/postgres.rs");
    let mut search_from = 0usize;
    let mut checked = 0usize;
    while let Some(relative) = source[search_from..].find("INSERT INTO") {
        let insert_start = search_from + relative;
        let Some(open_rel) = source[insert_start..].find('(') else {
            break;
        };
        let open = insert_start + open_rel;
        // The closing paren of the column list may be followed by a newline
        // before "VALUES", so walk forward until a ')' whose trailing text
        // (after whitespace) starts with "VALUES".
        let mut close = None;
        let mut cursor = open;
        while let Some(relative) = source[cursor..].find(')') {
            let candidate = cursor + relative;
            if source[candidate + 1..].trim_start().starts_with("VALUES") {
                close = Some(candidate);
                break;
            }
            cursor = candidate + 1;
        }
        let Some(close) = close else {
            break;
        };
        let columns = source[open + 1..close]
            .split(',')
            .map(str::trim)
            .filter(|column| !column.is_empty())
            .count();
        let values_marker = close + 1;
        let Some(vopen_rel) = source[values_marker..].find('(') else {
            break;
        };
        let vopen = values_marker + vopen_rel;
        let Some(vclose_rel) = source[vopen..].find(')') else {
            break;
        };
        let vclose = vopen + vclose_rel;
        let values = &source[vopen + 1..vclose];
        search_from = vclose;
        if values.contains("{placeholders}") || !values.contains('$') {
            continue;
        }
        let literal_count = values.matches('\'').count() / 2;
        let placeholder_indices = values
            .split('$')
            .skip(1)
            .filter_map(|part| {
                part.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<usize>()
                    .ok()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            columns,
            literal_count + placeholder_indices.len(),
            "INSERT column/placeholder imbalance in hand-written statement: {}",
            &values[..values.len().min(80)],
        );
        let mut sorted = placeholder_indices.clone();
        sorted.sort_unstable();
        for (expected, actual) in sorted.iter().enumerate() {
            assert_eq!(expected + 1, *actual, "placeholder gap in {values}");
        }
        checked += 1;
    }
    assert!(
        checked >= 8,
        "expected to scan at least 8 INSERT statements, scanned {checked}"
    );
}

#[tokio::test]
async fn persist_config_round_trips_through_postgres() {
    let _guard = env_lock().lock().await;
    let Some(database_url) = test_database_url() else {
        eprintln!("skipping postgres persist_config roundtrip: TEST_DATABASE_URL is not set");
        return;
    };
    reset_test_database(&database_url).await;

    let config = AppConfig::default();
    let state = AppState::load_from_database_url(&database_url, config.clone())
        .await
        .expect("should connect to the PostgreSQL test database");
    assert_eq!(state.config_store_backend_name(), "postgres");

    // One upstream per non-standard-field policy value: the boolean column
    // and the text column must both round-trip (Auto and AlwaysStrip both map
    // the boolean to true, Forward to false).
    for (id, policy) in [
        ("policy-auto", NonstandardFieldPolicy::Auto),
        ("policy-strip", NonstandardFieldPolicy::AlwaysStrip),
        ("policy-forward", NonstandardFieldPolicy::Forward),
    ] {
        state
            .insert_upstream(UpstreamConfig {
                id: id.into(),
                name: format!("{id} upstream"),
                base_url: format!("https://{id}.example"),
                api_key: format!("{id}-secret"),
                protocol: UpstreamProtocol::ChatCompletions,
                protocols: vec![UpstreamProtocol::ChatCompletions],
                supported_models: vec!["deepseek-v4-flash".into()],
                strip_nonstandard_chat_fields: policy,
                active: true,
                ..UpstreamConfig::default()
            })
            .await
            .expect("should persist upstream");
    }

    let downstream_key = generate_downstream_key("store-roundtrip");
    state
        .insert_downstream(DownstreamConfig {
            id: "downstream-store".into(),
            name: "Store roundtrip".into(),
            hash: downstream_key.hash,
            plaintext_key: Some(downstream_key.plaintext),
            plaintext_key_prefix: None,
            model_allowlist: vec![],
            ip_allowlist: vec![],
            rate_limit_enabled: true,
            per_minute_limit: 60,
            max_concurrency: 10,
            daily_token_limit: None,
            monthly_token_limit: None,
            input_token_price_per_million_cents: None,
            output_token_price_per_million_cents: None,
            daily_cost_limit_cents: None,
            request_quota_window_hours: None,
            request_quota_requests: None,
            expires_at: None,
            active: true,
            billing_mode: Default::default(),

            model_concurrency_groups: vec![],
        })
        .await
        .expect("should persist downstream");

    let current = state.runtime_settings_response().await;
    let mut settings = current.settings.clone();
    settings.app_name = "postgres-store-roundtrip".into();
    settings.upstream_http_pool_max_idle_per_host = 32;
    state
        .update_runtime_settings(current.revision, settings)
        .await
        .expect("should persist runtime settings");

    let reloaded = AppState::load_from_database_url(&database_url, config)
        .await
        .expect("should reload state from PostgreSQL");
    let snapshot = reloaded.snapshot().await;

    let policies: Vec<_> = snapshot
        .upstreams
        .iter()
        .map(|upstream| upstream.strip_nonstandard_chat_fields)
        .collect();
    // Upstreams are reloaded in id order ("policy-auto" < "policy-forward" <
    // "policy-strip").
    assert_eq!(
        policies,
        vec![
            NonstandardFieldPolicy::Auto,
            NonstandardFieldPolicy::Forward,
            NonstandardFieldPolicy::AlwaysStrip
        ]
    );
    assert_eq!(snapshot.downstreams.len(), 1);
    assert_eq!(snapshot.downstreams[0].id, "downstream-store");
    let reloaded_settings = reloaded
        .snapshot()
        .await
        .runtime_settings
        .expect("runtime settings should be persisted");
    assert_eq!(
        reloaded_settings.settings.app_name,
        "postgres-store-roundtrip"
    );
    assert_eq!(
        reloaded_settings
            .settings
            .upstream_http_pool_max_idle_per_host,
        32
    );
}
