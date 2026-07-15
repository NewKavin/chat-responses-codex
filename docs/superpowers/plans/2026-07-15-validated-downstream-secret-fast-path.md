# Validated Downstream Secret Fast Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve hash-authoritative downstream authentication while restoring the sub-50 ms request-path latency contract.

**Architecture:** Validate each stored plaintext/hash pair once at the AppState publication boundary. Clear invalid plaintext values, use constant-time comparison for validated pairs, and retain hash verification for records without plaintext.

**Tech Stack:** Rust 2021, Argon2, subtle constant-time equality, Axum integration tests.

---

### Task 1: Validate Stored Credential Pairs Before Publication

**Files:**
- Modify: `src/keys.rs`
- Modify: `src/state.rs`
- Modify: `src/server/gateway.rs`
- Modify: `tests/keys.rs`
- Modify: `tests/gateway/auth.rs`
- Modify: `tests/state_store.rs`

- [ ] **Step 1: Write failing validation and authentication tests**

Cover valid Argon2, mismatched Argon2, malformed Argon2, valid legacy,
mismatched legacy, and malformed legacy pairs. Construct `AppState` with a
matching plaintext plus mismatched hash and assert `downstream_for_secret`
rejects it and the normalized snapshot no longer exposes that plaintext.

Run: `rtk cargo test --locked --test keys --test state_store --test gateway auth:: -- --nocapture`

Expected: FAIL because current authentication either performs Argon2 on every
request or a plaintext helper can bypass an inconsistent hash.

- [ ] **Step 2: Add one credential-pair validation helper**

Add a helper in `src/keys.rs` that returns the stored plaintext only when
`verify_downstream_key(plaintext, hash)` succeeds. It must not expose a generic
plaintext-first authentication function.

- [ ] **Step 3: Normalize all AppState publication paths**

In all AppState constructors and in `mutate_persisted_state`, validate every
downstream pair before storing or persisting the candidate state. Clear invalid
plaintext and log only the downstream ID.

Add a private AppState authentication helper:

```rust
fn normalized_downstream_matches(downstream: &DownstreamConfig, candidate: &str) -> bool {
    downstream.plaintext_key.as_deref().map_or_else(
        || verify_downstream_key(candidate, &downstream.hash),
        |validated| validated.as_bytes().ct_eq(candidate.as_bytes()).into(),
    )
}
```

- [ ] **Step 4: Route every downstream authentication surface through AppState**

Use `downstream_for_secret` for inference, standard model listing, and Codex
catalog listing. Remove the public plaintext-aware matcher.

- [ ] **Step 5: Verify correctness and latency**

Run:

```bash
rtk cargo test --locked --test keys --test state_store --test gateway auth:: -- --nocapture
rtk cargo test --locked --test troubleshooting compatibility_matrix_records_first_meaningful_event_latency -- --nocapture
rtk cargo test --release --test load load_gateway_first_meaningful_event -- --ignored --exact --nocapture
rtk cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: all focused tests pass, troubleshooting records the expected delayed
event, and gateway-added release P95 remains below 50 ms.

- [ ] **Step 6: Commit the authentication boundary**

```bash
rtk git add src/keys.rs src/state.rs src/server/gateway.rs tests/keys.rs tests/gateway/auth.rs tests/state_store.rs docs/superpowers/specs/2026-07-15-validated-downstream-secret-fast-path-design.md docs/superpowers/plans/2026-07-15-validated-downstream-secret-fast-path.md
rtk git commit -m "fix(auth): validate downstream plaintext before fast matching"
```

