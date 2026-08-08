# Intranet Codex Reliability Master Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a staged reliability repair that keeps ten concurrent Codex TUI requests flowing across eight four-slot upstream accounts, preserves verified reasoning discovery, survives compatible-account `/resume` failover, and qualifies long contexts through the repository release scripts.

**Architecture:** Keep the existing route-health, account-recovery, capability-profile, response-history, and stream-commit boundaries. Correct the meanings passed between them: local admission is request-local scheduling, explicit provider concurrency owns account recovery, continuation is exact-route preferred rather than exact-route exclusive, operational probes preserve evidence, and context overflow has one protected pre-output compaction retry. Ship the five independently reversible stages in dependency order.

**Tech Stack:** Rust, Tokio, Axum, Reqwest, Redis Lua, PostgreSQL 15, Vue 3, TypeScript, Vitest, Docker Compose, Bash, Codex CLI.

---

## Plan Set And Ownership

- `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-admission-health.md`: exact-account request leases, typed upstream admission, Redis one-second hint, route-health cancellation, legacy state self-healing.
- `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-provider-classification.md`: semantic response precedence, 5xx concurrency recovery, context-wrapper identity.
- `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-continuation-failover.md`: versioned compatibility contract, preferred-route ordering, compatible-account failover.
- `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-capability-context.md`: revision-zero policy bootstrap, exact-route batch probes, model aggregation, protected context compaction.
- `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-release-qualification.md`: build/deploy script hardening, deterministic load, live Codex resume, long-context qualification, evidence capture.

The production implementation must not introduce cross-plan duplicate types. `UpstreamAdmissionRejectionReason` belongs to `src/state.rs`; `UpstreamResponseSemantic` belongs to `src/upstream_feedback.rs`; `ContinuationCompatibilityContract` belongs to `src/server/gateway/capability_routing.rs`.

## Acceptance Coverage

| Criterion | Owning plan | Required evidence |
| --- | --- | --- |
| AC1: 1,000 requests, concurrency 10, eight accounts x four | admission + release | deterministic soak JSON, aggregate physical in-flight reaches 10, zero logical 429/502/503, each account <= 4 |
| AC2: two concurrency 502 accounts | classification + release | gateway integration and soak scenario complete through six healthy routes |
| AC3: all accounts locally full for three seconds | admission + release | queued completion after release, no route-health snapshot |
| AC4: generic failure is bounded | classification | stable terminal category and bounded elapsed time |
| AC5: resumed tool/reasoning session fails over | continuation + release | alternate route receives replay once; automated resume and real TUI `/resume` complete |
| AC6: verified DeepSeek/GLM levels survive route failure | capability + release | model-level union plus route-level operational diagnostics |
| AC7: serial Codex TUI matrix has no logical 429/502/503 | capability/context + release | TUI transcript, JSONL, and PostgreSQL usage assertions |
| AC8: delayed stream has no 499 or replay | classification + release | delayed-output script and one terminal usage row |
| AC9: defective 24-hour cooldown is selectively removed | admission | Redis keys outside the legacy predicate remain byte-for-byte unchanged |
| AC10: revision zero bootstraps, nonzero is unchanged | capability | persistence round-trip test and deployment export digest |
| AC11: no excessive statusless concurrency cooldown | admission + release | post-soak invariant report count is zero |
| AC12: complete release evidence | release | image digest, health, logs, Redis, PostgreSQL, Codex JSONL manifest |

### Task 1: Establish The Baseline And Stage Branches

**Files:**
- Read: `docs/superpowers/specs/2026-08-08-intranet-codex-reliability-design.md`
- Read: the five child plans listed above

- [ ] **Step 1: Verify the design and plans are the committed baseline**

Run:

```bash
rtk git status --short
rtk git log -1 --oneline aea1d77
rtk rg -n 'T[D]O|T[B]D|implement l[a]ter|fill in d[e]tails|Similar to T[a]sk' docs/superpowers/plans/2026-08-08-intranet-codex-reliability-*.md
```

Expected: clean worktree before implementation, design commit `aea1d77`, and no placeholder matches.

- [ ] **Step 2: Run the pre-change focused baseline**

Run:

```bash
rtk cargo test --lib upstream_feedback
rtk cargo test --test redis_runtime redis_main_upstream_concurrency
rtk cargo test --test gateway responses_continuation_operational_failure_does_not_try_a_different_profile
rtk cargo test --test capability_probe responses_reasoning_control_probe_uses_nested_reasoning_effort
rtk cargo test --test gateway context_limit_error_retries_once_with_reduced_max_tokens
rtk npm --prefix frontend test -- --run
```

Expected: current tests pass. Save the output with the release evidence; the new RED tests in child plans must fail for the stated behavioral reason rather than because the baseline is broken.

### Task 2: Execute Stage 1, Admission Correctness

**Files:**
- Execute: `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-admission-health.md`

- [ ] **Step 1: Complete every admission-plan checkbox and commit**

Expected commit subject:

```text
fix(runtime): scope request leases to exact accounts
fix(runtime): keep local admission out of route health
```

- [ ] **Step 2: Run the Stage 1 gate**

```bash
rtk cargo test --lib reservation_capacity_rejection_is_request_local_and_does_not_cool_route
rtk cargo test --lib local_upstream_concurrency_is_scoped_per_account
rtk cargo test --lib all_accounts_locally_full_then_release_without_route_cooldown
rtk cargo test --test redis_runtime redis_upstream_concurrency_is_scoped_per_account
rtk cargo test --test redis_runtime redis_main_upstream_concurrency_uses_optimistic_retry_hint
rtk cargo test --test redis_runtime redis_gateway_local_capacity_release_is_immediately_schedulable
rtk cargo test --test redis_runtime legacy_local_admission_route_health_is_repaired_selectively
```

Expected: all pass without sleeps used to wait out a route cooldown.

### Task 3: Execute Stage 2, Provider Classification

**Files:**
- Execute: `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-provider-classification.md`

- [ ] **Step 1: Complete every classification-plan checkbox and commit**

Expected commit subject:

```text
fix(routing): recover explicit concurrency wrapped in 5xx
```

- [ ] **Step 2: Run the Stage 2 gate**

```bash
rtk cargo test --lib upstream_feedback
rtk cargo test --test gateway explicit_concurrency_5xx_uses_account_recovery_and_healthy_routes
rtk cargo test --test gateway generic_503_remains_bounded_transient_failure
rtk cargo test --test gateway explicit_context_wrappers_do_not_cool_route
```

Expected: explicit concurrency 502/503 uses the configured account wait budget; generic 5xx does not.

### Task 4: Execute Stage 3, Resume Resilience

**Files:**
- Execute: `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-continuation-failover.md`

- [ ] **Step 1: Complete every continuation-plan checkbox and commit**

Expected commit subject:

```text
fix(responses): fail over compatible continuations before output
```

- [ ] **Step 2: Run the Stage 3 gate**

```bash
rtk cargo test --test gateway successful_continuation_failover_updates_preferred_profile
rtk cargo test --test gateway responses_continuation_503_fails_over_to_compatible_account
rtk cargo test --test gateway responses_continuation_rejects_each_contract_mismatch
rtk cargo test --test gateway responses_continuation_after_semantic_output_never_replays
rtk cargo test --test postgres_roundtrip response_history
```

Expected: exact route stays first, compatible route is used only before semantic output, and stored history remains backward compatible.

### Task 5: Execute Stage 4, Capability And Context Reliability

**Files:**
- Execute: `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-capability-context.md`

- [ ] **Step 1: Complete every capability/context checkbox and commit in its prescribed units**

Expected final Stage 4 subjects:

```text
fix(capabilities): queue one-click probes as exact route batches
fix(context): preserve unresolved tools and recent reasoning
fix(context): compact once on explicit overflow before output
```

- [ ] **Step 2: Run the Stage 4 gate**

```bash
rtk cargo test --test admin capability_probe_all
rtk cargo test --test capability_probe operational_failure_preserves_prior_reasoning_evidence_and_schedules_retry
rtk cargo test --test gateway codex_catalog_unions_current_verified_route_levels
rtk cargo test --test gateway context_overflow_503_compacts_once_without_cooling_route
rtk cargo test --test gateway context_compaction_preserves_unresolved_tool_pairs_and_recent_reasoning
rtk npm --prefix frontend test -- --run
rtk npm --prefix frontend run type-check
```

Expected: one failed route cannot erase model-level evidence; unsafe context entries are never compacted.

### Task 6: Execute Stage 5 And Produce Release Evidence

**Files:**
- Execute: `docs/superpowers/plans/2026-08-08-intranet-codex-reliability-release-qualification.md`

- [ ] **Step 1: Complete repository verification and create a versioned image**

Run the exact verification and build commands from the release plan. The image tag must be immutable, for example:

```bash
rtk scripts/build-package-image.sh --image chat-responses-codex --tag 2026-08-08-reliability.1 --output artifacts/chat-responses-codex-2026-08-08-reliability.1.tar
```

Expected: release binary, image, tar, and SHA-256 manifest exist.

- [ ] **Step 2: Deploy only through the repository script**

```bash
rtk scripts/deploy.sh --deploy-dir /home/kavin/docker/chat-responses-codex --image chat-responses-codex --tag 2026-08-08-reliability.1
```

Expected: the script replaces the gateway, waits for health, reports the startup migration summary, and never overwrites the existing `.env` or PostgreSQL/Redis volumes.

- [ ] **Step 3: Run the complete qualification matrix**

Run the release-plan commands for deterministic Redis, eight-account soak, installed Codex, automated persisted resume, interactive TUI `/resume`, delayed output, and serial context tiers.

Expected: every required artifact is indexed by `artifacts/reliability-2026-08-08/manifest.json` and all 12 acceptance checks are `passed`.

- [ ] **Step 4: Commit release automation separately**

```bash
rtk git add scripts docker-compose.yml .env.example DEPLOYMENT.md tests
rtk git commit -m "test(reliability): qualify intranet Codex release path" -m "Constraint: Build and deployment use repository scripts only" -m "Confidence: high" -m "Scope-risk: moderate"
```

### Task 7: Final Cross-Stage Verification

**Files:**
- Verify: all files touched by the child plans

- [ ] **Step 1: Run repository-wide checks**

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test --all-targets --all-features
rtk npm --prefix frontend test -- --run
rtk npm --prefix frontend run type-check
rtk npm --prefix frontend run build
rtk bash -n scripts/*.sh
rtk docker compose --env-file .env.example config --quiet
```

Expected: every command exits zero. Redis-dependent tests may use their existing serialized environment but must not be silently skipped in release evidence.

- [ ] **Step 2: Check invariants and worktree**

```bash
rtk rg -n 'RouteFailureWithRetry.*ConcurrencySaturated' src/server/gateway.rs
rtk rg -n "oldest\[2\].*now_ms" src/state/redis_runtime/upstream_reserve.lua
rtk rg -n 'T[D]O|T[B]D|implement l[a]ter|fill in d[e]tails' docs/superpowers/plans/2026-08-08-intranet-codex-reliability-*.md
rtk git status --short
```

Expected: the first two searches have no match, placeholder search has no match, and only intentional release artifacts remain untracked.
