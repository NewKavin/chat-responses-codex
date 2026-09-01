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
    // C2.3: push the stale threshold far past the TTL so this test pins the
    // expiry-based reclamation (`leaked_reclaimed_total`) instead of the stale
    // path — the stale path has its own dedicated test below.
    let config = AppConfig {
        upstream_lease_stale_after_ms: 86_400_000,
        ..AppConfig::default()
    };
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

/// E3 (§3.5 / §5.2): the local-concurrency Retry-After must be estimated from
/// observed *hold* durations (release − reserve), not from the lease TTL.  The
/// pre-E3 code used `oldest_remaining_secs`; with the C2 heartbeat keeping
/// live leases topped up to ~full TTL (300s) that number was pinned near 300
/// and the 30s cap flattened it to a constant — exactly the "retried for 32s
/// across 6 rounds" artifact while the upstream was never even contacted.
///
/// With no hold samples there is no observation to lean on, so the estimate
/// falls back to the static first-probe-delay floor (1s), not 30.
#[tokio::test(start_paused = true)]
async fn e3_retry_after_without_samples_is_not_the_constant_cap() {
    let upstream = test_upstream("e3-no-samples");
    let (state, _directory) = test_state(&upstream);
    let account = ("e3-no-samples".to_string(), "fingerprint-e3ns".to_string());

    // Fill the single slot and hold it, renewing the heartbeat exactly like a
    // long stream would (C2 keeps the TTL near 300s).  Pre-E3 the rejection
    // below advertised ~30s (300s remaining capped); E3 must not read the TTL.
    let held = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .unwrap();
    for _ in 0..3u32 {
        advance(Duration::from_secs(80)).await;
        state.renew_upstream_request(&held).await.unwrap();
    }

    let rejected = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .expect_err("the single slot is still pinned by the stream");
    assert!(matches!(
        rejected.reason,
        UpstreamAdmissionRejectionReason::LocalConcurrency
    ));
    assert!(
        rejected.retry_after_seconds <= 5,
        "E3: with no hold samples the estimate must be the static probe floor, \
         not the renewed-TTL/capped constant; got {}s",
        rejected.retry_after_seconds
    );
    assert_ne!(
        rejected.retry_after_seconds, 30,
        "E3: the old 30s constant (renewed TTL capped) must be gone"
    );

    // Release the stream: its real 240s of service time becomes one sample.
    state.release_upstream_request(held).await.unwrap();
}

/// E3: with a known hold-duration sample set, the estimate must land where
/// §3.5 predicts: `p50_hold − oldest_lease_already_held`, floored by the
/// probe delay.  This pins the formula against regressions back to TTL maths.
#[tokio::test(start_paused = true)]
async fn e3_retry_after_tracks_observed_hold_duration() {
    let upstream = test_upstream("e3-known-holds");
    let (state, _directory) = test_state(&upstream);
    let account = ("e3-known-holds".to_string(), "fingerprint-e3kh".to_string());

    // Build two observed holds: 60s then 40s => p50 (sorted [40,60]) = 60s.
    let first = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .unwrap();
    advance(Duration::from_secs(60)).await;
    state.release_upstream_request(first).await.unwrap();

    let second = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .unwrap();
    advance(Duration::from_secs(40)).await;
    state.release_upstream_request(second).await.unwrap();

    // Third request: hold it 20s and ask for a new reservation.  The oldest
    // occupant has already consumed 20s of the 60s p50 window, so the slot
    // frees in ~40s.
    let occupant = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .unwrap();
    advance(Duration::from_secs(20)).await;

    let rejected = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .expect_err("the single slot is occupied");
    assert!(matches!(
        rejected.reason,
        UpstreamAdmissionRejectionReason::LocalConcurrency
    ));
    assert_eq!(
        rejected.retry_after_seconds, 40,
        "E3: p50_hold (60s) minus already-held (20s), not any TTL-derived value"
    );

    state.release_upstream_request(occupant).await.unwrap();
}

/// E3: a request that was held *longer* than the p50 window must not produce a
/// negative retry-after — "the slot is already overdue" means "retry ~now",
/// floored at the probe delay.
#[tokio::test(start_paused = true)]
async fn e3_retry_after_floors_at_probe_delay_when_overdue() {
    let upstream = test_upstream("e3-overdue");
    let (state, _directory) = test_state(&upstream);
    let account = ("e3-overdue".to_string(), "fingerprint-e3od".to_string());

    // Two quick samples: p50 = 2s.
    for hold_secs in [1u64, 3] {
        let lease = state
            .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
            .await
            .unwrap();
        advance(Duration::from_secs(hold_secs)).await;
        state.release_upstream_request(lease).await.unwrap();
    }

    // The next occupant is already 120s in — far past p50 — so the first slot
    // is overdue; the estimate must bottom out at the probe floor (1s).
    let occupant = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .unwrap();
    advance(Duration::from_secs(120)).await;

    let rejected = state
        .try_reserve_upstream_account_request(&upstream, &account.1, "model-a")
        .await
        .expect_err("the single slot is occupied");
    assert_eq!(
        rejected.retry_after_seconds, 1,
        "E3: overdue hold must floor at the probe delay"
    );
    state.release_upstream_request(occupant).await.unwrap();
}

/// E4.1 migration safety: changing the factory default `max_concurrency` from
/// 4 → 32 must only affect *newly created* upstreams.  Persisted upstreams
/// store the explicit value; serde's `default` only kicks in when the field is
/// absent from the stored JSON.  (The E7 migration docs tell operators to use
/// `POST /api/admin/upstreams/batch-update` to adjust existing upstreams.)
#[test]
fn e4_1_persisted_max_concurrency_survives_default_change() {
    // A persisted upstream that pinned max_concurrency = 4 stays 4 across a
    // serialization round-trip (the old default's value, now different).
    let persisted = UpstreamConfig {
        id: "persisted-e4".into(),
        name: "persisted".into(),
        max_concurrency: 4,
        ..UpstreamConfig::default()
    };
    let json = serde_json::to_string(&persisted).unwrap();
    let back: UpstreamConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.max_concurrency, 4,
        "persisted explicit max_concurrency must win over the new default"
    );

    // A freshly created upstream (no explicit field in its stored JSON) takes
    // the E4.1 default of 32.
    let mut fresh: serde_json::Value = serde_json::to_value(UpstreamConfig {
        id: "fresh-e4".into(),
        name: "fresh".into(),
        ..UpstreamConfig::default()
    })
    .unwrap();
    fresh.as_object_mut().unwrap().remove("max_concurrency");
    let fresh_back: UpstreamConfig = serde_json::from_value(fresh).unwrap();
    assert_eq!(
        fresh_back.max_concurrency, 32,
        "a field-less upstream JSON must deserialize to the new E4.1 default"
    );

    // And the in-code default itself is 32 (the admin runtime-setting surface
    // already asserts `default_upstream_max_concurrency == 32`).
    assert_eq!(
        chat_responses_codex::state::default_upstream_max_concurrency(),
        32
    );
}

/// E4.3: `local_slot_queue_plan` derives the adaptive queue budget from the
/// observed hold percentiles.  Before E4.3 it fed `hold_p95_seconds` (whole
/// **seconds**) straight into a **millisecond** clamp, so `p95 × 1.5` could
/// only clear a 10s floor at a p95 above 6666 s (~1.85 h): the budget was
/// pinned to the floor and the whole adaptive path was dead code, and the
/// slower the model the more useless it got.  This pins the converted maths.
///
/// The holds are produced through the real reserve/release path on a paused
/// clock, so the percentiles come from genuine lease accounting rather than a
/// hand-seeded table.
#[tokio::test(start_paused = true)]
async fn adaptive_queue_budget_scales_with_observed_hold_in_milliseconds() {
    let mut upstream = test_upstream("adaptive-budget");
    // One slot, so each reserve/release pair is strictly sequential and the
    // hold sample is exactly the advanced duration.
    upstream.max_concurrency = 1;
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream.clone()]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            // Floor 10s (the shipped default) and an explicit factor/ceiling so
            // the assertion does not silently follow a changed default.
            upstream_account_queue_max_wait_ms: 10_000,
            upstream_account_queue_adaptive_budget_factor: 1.5,
            upstream_account_queue_adaptive_budget_ceiling_ms: 60_000,
            upstream_account_queue_skip_when_doomed_enabled: true,
            // Keep the TTL well past the holds so nothing is reclaimed as stale
            // mid-test.
            upstream_local_lease_ttl_seconds: 3_600,
            upstream_lease_stale_after_ms: 86_400_000,
            ..AppConfig::default()
        },
    );
    let fingerprint = "fingerprint-adaptive";
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        upstream.id.clone(),
        fingerprint.to_string(),
    );

    // Fewer than two samples ⇒ fall back to the static floor, no skip.
    let (budget, skip) = state.local_slot_queue_plan(&account).await;
    assert_eq!(
        (budget, skip),
        (10_000, false),
        "with no hold samples the plan must fall back to the static floor"
    );

    // Four holds: 20s, 20s, 20s, 30s ⇒ p50 = 20s, p95 = 30s.
    for hold_seconds in [20_u64, 20, 20, 30] {
        let lease = state
            .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
            .await
            .expect("the single slot must be free between sequential holds");
        advance(Duration::from_secs(hold_seconds)).await;
        state
            .release_upstream_request(lease)
            .await
            .expect("release must record the hold sample");
    }

    let (budget, skip) = state.local_slot_queue_plan(&account).await;
    // p95 = 30s → 30_000ms × 1.5 = 45_000ms, inside [10_000, 60_000].
    // Pre-E4.3 this was `30 × 1.5 = 45` clamped up to the 10_000 floor.
    assert_eq!(
        budget, 45_000,
        "the budget must be p95(30s)×1.5 = 45s in ms, not the floor"
    );
    // p50 = 20s outlasts the 10s floor ⇒ the E4.2 skip still fires.
    assert!(
        skip,
        "median hold (20s) beyond the 10s floor must still mark the wait doomed"
    );

    // E4.3: the skip is switchable, and turning it off must not disturb the
    // budget — a slow-model deployment wants to queue, not be rejected locally.
    let mut settings = (*state.runtime_settings()).clone();
    settings.upstream_account_queue_skip_when_doomed_enabled = false;
    state
        .update_runtime_settings(0, settings)
        .await
        .expect("runtime settings update must apply");
    let (budget, skip) = state.local_slot_queue_plan(&account).await;
    assert_eq!(
        budget, 45_000,
        "the budget is independent of the skip switch"
    );
    assert!(
        !skip,
        "with the skip disabled a doomed-looking wait must still queue"
    );
}

/// E4.3: the adaptive plan must read the *live* runtime settings, not the
/// static startup config.  Before E4.3 `local_slot_queue_plan` read
/// `self.config` while the gateway's non-adaptive branch read the runtime
/// settings, so hot-reloading `upstream_account_queue_max_wait_ms` silently
/// did nothing on the adaptive path and an operator raising the wait limit
/// saw no effect at all.
#[tokio::test(start_paused = true)]
async fn hot_reloaded_wait_floor_moves_the_adaptive_budget() {
    let mut upstream = test_upstream("hot-floor");
    upstream.max_concurrency = 1;
    let directory = tempdir().unwrap();
    let state = AppState::new(
        PersistedState {
            upstreams: Arc::new(vec![upstream.clone()]),
            ..Default::default()
        },
        directory.path().join("state.json"),
        AppConfig {
            upstream_account_queue_max_wait_ms: 10_000,
            upstream_account_queue_adaptive_budget_factor: 1.5,
            upstream_account_queue_adaptive_budget_ceiling_ms: 180_000,
            upstream_account_queue_skip_when_doomed_enabled: false,
            upstream_local_lease_ttl_seconds: 3_600,
            upstream_lease_stale_after_ms: 86_400_000,
            ..AppConfig::default()
        },
    );
    let fingerprint = "fingerprint-hot-floor";
    let account = chat_responses_codex::state::AccountConcurrencyKey::new(
        upstream.id.clone(),
        fingerprint.to_string(),
    );

    // No samples yet: the plan returns the configured floor verbatim, which is
    // the cheapest observation of which config source it read.
    assert_eq!(
        state.local_slot_queue_plan(&account).await,
        (10_000, false),
        "the plan must start from the configured floor"
    );

    // Hot-reload a larger floor.  Nothing else changes.
    let mut settings = (*state.runtime_settings()).clone();
    settings.upstream_account_queue_max_wait_ms = 45_000;
    state
        .update_runtime_settings(0, settings)
        .await
        .expect("raising the wait floor must be accepted");

    assert_eq!(
        state.local_slot_queue_plan(&account).await,
        (45_000, false),
        "the hot-reloaded floor must take effect on the adaptive path (pre-E4.3 this stayed 10_000)"
    );

    // With samples present the new floor still governs: two 2s holds give
    // p95 = 2s, so p95_ms x 1.5 = 3_000 is below the 45s floor and clamps up.
    for _ in 0..2 {
        let lease = state
            .try_reserve_upstream_account_request(&upstream, fingerprint, "model-a")
            .await
            .expect("the single slot must be free between sequential holds");
        advance(Duration::from_secs(2)).await;
        state.release_upstream_request(lease).await.unwrap();
    }
    let (budget, skip) = state.local_slot_queue_plan(&account).await;
    assert_eq!(
        budget, 45_000,
        "a p95-derived budget below the hot-reloaded floor must clamp up to it"
    );
    assert!(!skip, "the skip switch stays off regardless of the floor");
}
