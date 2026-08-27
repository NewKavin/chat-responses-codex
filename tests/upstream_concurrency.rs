//! Local upstream account concurrency lease lifecycle (P7):
//! leak reclamation, long-stream protection via renewal, sync expire fallback.
//!
//! All tests use `start_paused = true` so the lease TTL can be advanced
//! deterministically without real sleeps (mirrors route_health tests).

use chat_responses_codex::state::{
    AppConfig, AppState, PersistedState, UpstreamAdmissionRejectionReason, UpstreamConfig,
};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::time::{advance, Duration};

fn test_upstream(id: &str) -> UpstreamConfig {
    UpstreamConfig {
        id: id.into(),
        name: "local lease test".into(),
        active: true,
        max_concurrency: 1,
        ..UpstreamConfig::default()
    }
}

fn test_state(upstream: &UpstreamConfig) -> (AppState, tempfile::TempDir) {
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream.clone()]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig::default(),
    );
    (state, directory)
}

#[tokio::test(start_paused = true)]
async fn leaked_lease_is_reclaimed_after_local_ttl() {
    let upstream = test_upstream("leak-reclaim");
    let mut config = AppConfig::default();
    // C2.3: push the stale threshold far past the TTL so this test pins the
    // expiry-based reclamation (`leaked_reclaimed_total`) instead of the stale
    // path — the stale path has its own dedicated test below.
    config.upstream_lease_stale_after_ms = 86_400_000;
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream.clone()]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        config,
    );
    let account = ("leak-reclaim".to_string(), "fingerprint-leak".to_string());

    let lease = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .unwrap();
    // Simulate a guard dropped outside the Tokio runtime: the release path
    // never runs, the lease id stays in the local map forever (pre-P7).
    std::mem::forget(lease);

    // Capacity is pinned until the TTL lapses.
    let blocked = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .expect_err("the leaked lease must pin the single slot");
    assert!(matches!(
        blocked.reason,
        UpstreamAdmissionRejectionReason::LocalConcurrency
    ));

    // Advance past the default local lease TTL (300s): the slot must free.
    // C2.3 stale reclamation is disabled for this test (stale_after pushed
    // past the TTL) so it exercises the expiry path (`leaked_reclaimed_total`)
    // rather than the stale path (`stale_reclaimed_total`), which has its own
    // dedicated test below.
    advance(Duration::from_secs(361)).await;

    let replacement = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .expect("the expired lease must be reclaimed lazily");
    state.release_upstream_request(replacement).await.unwrap();

    let snapshots = state.upstream_runtime_snapshots().await.unwrap();
    let snapshot = snapshots.get(&upstream.id).unwrap();
    assert_eq!(
        snapshot.in_flight, 0,
        "in_flight must be zero after reclamation"
    );
    assert_eq!(
        snapshot.leaked_reclaimed_total, 1,
        "reclamation must be observable"
    );
}

/// A long stream must never have its slot reclaimed while it is still
/// running: renewal (the counterpart of `renew_downstream_concurrency`)
/// refreshes the lease expiry before the TTL lapses.  This is the guard
/// against P7's TTL over-reclaiming a live request.
#[tokio::test(start_paused = true)]
async fn long_stream_lease_is_renewed_before_ttl_expiry() {
    let upstream = test_upstream("long-stream-renew");
    let (state, _directory) = test_state(&upstream);
    let fingerprint = "fingerprint-long";

    let lease = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .unwrap();

    // Half the default TTL elapses; the stream is still producing chunks.
    advance(Duration::from_secs(150)).await;
    state
        .renew_upstream_request(&lease)
        .await
        .expect("renewal must succeed while the stream is live");

    // Past the original TTL but the lease was renewed: the slot must still be
    // pinned (a fresh request for the same account is rejected).
    advance(Duration::from_secs(151)).await;
    let blocked = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .expect_err("renewed lease must still pin the slot");
    assert!(matches!(
        blocked.reason,
        UpstreamAdmissionRejectionReason::LocalConcurrency
    ));

    // Renewal is idempotent: a second renew is a no-op success, and once the
    // lease is released the slot frees for the next request.
    state.renew_upstream_request(&lease).await.unwrap();
    state.release_upstream_request(lease).await.unwrap();
    let replacement = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .expect("the released lease must free the slot immediately");
    state.release_upstream_request(replacement).await.unwrap();
}

/// The synchronous fallback used by `spawn_release` when the guard drops
/// outside a Tokio runtime: the slot must free immediately, not after a TTL.
#[tokio::test(start_paused = true)]
async fn expire_upstream_request_lease_sync_reclaims_without_runtime() {
    let upstream = test_upstream("sync-expire");
    let (state, _directory) = test_state(&upstream);
    let fingerprint = "fingerprint-sync";

    let lease = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .unwrap();
    assert_eq!(
        state
            .upstream_runtime_snapshots()
            .await
            .unwrap()
            .get(&upstream.id)
            .unwrap()
            .in_flight,
        1
    );

    // Simulate the Drop-without-runtime path: no async release, no advance.
    assert!(
        state.expire_upstream_request_lease_sync(&lease),
        "the lease must be removed synchronously"
    );
    assert!(
        !state.expire_upstream_request_lease_sync(&lease),
        "a second sync expire must be a no-op"
    );

    let replacement = state
        .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
        .await
        .expect("sync-expired lease must free the slot immediately");
    state.release_upstream_request(replacement).await.unwrap();
    let snapshots = state.upstream_runtime_snapshots().await.unwrap();
    assert_eq!(snapshots.get(&upstream.id).unwrap().in_flight, 0);
}
/// C2.3: a lease whose heartbeat stops (the holder is gone) is reclaimed by
/// the stale sweep after `upstream_lease_stale_after_ms` — well before the TTL
/// (300s) lapses — and counted separately from expiry-based reclamation.
#[tokio::test(start_paused = true)]
async fn leaked_lease_is_reclaimed_as_stale_before_ttl() {
    let upstream = test_upstream("stale-reclaim");
    let (state, _directory) = test_state(&upstream);
    let account = ("stale-reclaim".to_string(), "fingerprint-stale".to_string());

    let lease = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .unwrap();
    // No release and no heartbeat — the holder is gone.  Default stale_after
    // is 200s (2x the ttl/3 heartbeat), the default TTL is 300s.
    std::mem::forget(lease);

    // Advance past stale_after but well short of the TTL.
    advance(Duration::from_secs(201)).await;

    let replacement = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .expect("the stale lease must be reclaimed before the TTL lapses");
    state.release_upstream_request(replacement).await.unwrap();

    let snapshots = state.upstream_runtime_snapshots().await.unwrap();
    let snapshot = snapshots.get(&upstream.id).unwrap();
    assert_eq!(
        snapshot.in_flight, 0,
        "in_flight must be zero after reclamation"
    );
    assert_eq!(
        snapshot.stale_reclaimed_total, 1,
        "the stale sweep must count separately"
    );
    assert_eq!(
        snapshot.leaked_reclaimed_total, 0,
        "the expiry counter must stay untouched"
    );
}
