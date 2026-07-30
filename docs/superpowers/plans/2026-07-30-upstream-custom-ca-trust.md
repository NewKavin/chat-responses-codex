# Upstream Custom CA Trust Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow every upstream HTTP operation to trust one PEM bundle or a directory of internal CA certificates while preserving strict TLS verification and useful model-discovery errors.

**Architecture:** Add a focused `UpstreamCaConfig` loader that validates the configured source once at startup and stores parsed Reqwest certificates. Apply the same additive roots to the normal and no-proxy clients owned by `AppState`, then make all administrative discovery paths reuse those clients. Keep deployment certificates in an ignored, read-only `certs/` directory and format safe backend errors through a tested frontend helper.

**Tech Stack:** Rust 2021, Reqwest 0.12 with Rustls, Axum, Tokio, Vue 3, TypeScript, Vitest, Docker Compose.

---

## File Structure

- Create `src/upstream_tls.rs`: validate a file or directory CA source and expose parsed certificates without exposing their contents.
- Modify `src/lib.rs`: export the new TLS configuration module.
- Modify `src/state/types.rs`: add the runtime-only `UpstreamCaConfig` field to `AppConfig` with an empty default.
- Modify `src/main.rs`: load `UPSTREAM_CA_CERT_PATH` before constructing application state.
- Modify `src/util.rs`: append validated roots to both upstream client builders and remove the silent configured-root fallback.
- Modify `src/state/model_discovery.rs`: expose the canonical model-discovery URL helper.
- Modify `src/server/admin.rs`: reuse `AppState` clients for manual discovery, batch discovery, and model probes.
- Create `tests/common/tls.rs` and modify `tests/common/mod.rs`: provide one generated private-CA TLS model server shared by integration tests.
- Create `tests/upstream_ca.rs`: CA file/directory validation and real private-CA TLS coverage.
- Modify `tests/admin_upstreams.rs`: prove the admin discovery route uses configured trust.
- Modify `frontend/src/api/admin.ts` and `frontend/tests/api/admin.spec.ts`: add and test safe all-key failure formatting.
- Modify `frontend/src/views/admin/Upstreams.vue`: display the generic summary plus indexed backend errors.
- Create `certs/.gitignore` and `certs/README.md`: keep the mount point while excluding environment certificates.
- Modify `.env.example`, `docker-compose.yml`, `DEPLOYMENT.md`, and `tests/docker.rs`: document and verify deployment wiring.

### Task 1: Load And Apply Custom CA Certificates

**Files:**
- Create: `src/upstream_tls.rs`
- Modify: `src/lib.rs`
- Modify: `src/state/types.rs`
- Modify: `src/main.rs`
- Modify: `src/util.rs`
- Modify: `Cargo.toml`
- Create: `tests/common/tls.rs`
- Modify: `tests/common/mod.rs`
- Test: `tests/upstream_ca.rs`

- [ ] **Step 1: Write failing CA loader tests**

Create tests that construct CA material with `rcgen`, then assert this public API:

```rust
use chat_responses_codex::upstream_tls::UpstreamCaConfig;

#[test]
fn loads_sorted_pem_and_crt_files_from_directory() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("b.pem"), generated_ca_pem("ca-b")).unwrap();
    std::fs::write(directory.path().join("a.crt"), generated_ca_pem("ca-a")).unwrap();
    std::fs::write(directory.path().join("README.md"), "ignored").unwrap();

    let config = UpstreamCaConfig::load(Some(directory.path())).unwrap();

    assert_eq!(config.len(), 2);
    assert!(config.is_configured());
}

#[test]
fn rejects_an_empty_configured_directory() {
    let directory = tempfile::tempdir().unwrap();
    let error = UpstreamCaConfig::load(Some(directory.path())).unwrap_err();
    assert!(error.to_string().contains("no .crt or .pem certificates"));
}
```

Also cover a multi-certificate bundle file, missing path, invalid selected file, and ignored extensions. Add `rcgen = "0.14"` and `tokio-rustls = "0.26"` as dev dependencies.

- [ ] **Step 2: Run loader tests and verify RED**

Run:

```bash
rtk cargo test --test upstream_ca -- --nocapture
```

Expected: Cargo resolves the new dev dependencies and compilation then fails because `upstream_tls::UpstreamCaConfig` does not exist. Subsequent test commands use the updated lock file with `--locked --offline`.

- [ ] **Step 3: Implement the validated loader**

Create a cloneable runtime-only type with this interface:

```rust
#[derive(Clone, Default)]
pub struct UpstreamCaConfig {
    configured_path: Option<PathBuf>,
    certificates: Vec<reqwest::Certificate>,
}

impl UpstreamCaConfig {
    pub fn load(path: Option<&Path>) -> io::Result<Self>;
    pub fn certificates(&self) -> &[reqwest::Certificate];
    pub fn is_configured(&self) -> bool;
    pub fn len(&self) -> usize;
}
```

For a directory, sort entries, accept regular `.crt` and `.pem` files case-insensitively, parse each with `reqwest::Certificate::from_pem_bundle`, and require at least one certificate. Return `io::ErrorKind::InvalidInput` with the path for invalid or empty sources, never certificate bytes.

Add the runtime-only config to `AppConfig`:

```rust
#[serde(skip)]
pub upstream_ca: UpstreamCaConfig,
```

Load it in `main` from a trimmed, non-empty `UPSTREAM_CA_CERT_PATH`, and initialize `AppConfig::default()` with `UpstreamCaConfig::default()`.

Apply every certificate before `ClientBuilder::build()`:

```rust
for certificate in config.upstream_ca.certificates() {
    builder = builder.add_root_certificate(certificate.clone());
}
```

If client construction fails, panic with a bounded configuration error instead of creating a new client that omits configured roots.

- [ ] **Step 4: Add a real TLS trust test**

Create the reusable TLS fixture in `tests/common/tls.rs`: use `rcgen` to create a CA and a `localhost` server certificate, then serve `GET /v1/models` through `tokio-rustls`. Export the CA PEM and bound base URL. In `tests/upstream_ca.rs`, build clients through `build_upstream_http_client`; assert the unconfigured client rejects the chain and the configured client returns `200` with `data[].id`.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test upstream_ca -- --nocapture
rtk cargo test --locked --offline --test admin_upstreams test_admin_discover_upstream_models -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 6: Commit CA loading**

```bash
rtk git add Cargo.toml Cargo.lock src/lib.rs src/main.rs src/state/types.rs src/upstream_tls.rs src/util.rs tests/common/mod.rs tests/common/tls.rs tests/upstream_ca.rs
rtk git commit -m "feat(tls): load internal upstream CA certificates"
```

### Task 2: Reuse Trusted Clients For Administrative Upstream Calls

**Files:**
- Modify: `src/state/model_discovery.rs`
- Modify: `src/server/admin.rs`
- Modify: `tests/admin_upstreams.rs`
- Modify: `tests/common/tls.rs`
- Test: `tests/upstream_ca.rs`

- [ ] **Step 1: Write a failing admin discovery TLS test**

Construct `AppState` with `config.upstream_ca` loaded from the generated private CA, route an authenticated request to `/api/admin/upstreams/discover-models`, and assert:

```rust
assert_eq!(response.status(), StatusCode::OK);
assert_eq!(body["failed"], 0);
assert_eq!(body["models"], json!(["internal-model"]));
```

The test must use the TLS server from Task 1 so it fails while the handler still constructs an independent default client.

- [ ] **Step 2: Run the admin test and verify RED**

Run:

```bash
rtk cargo test --locked --offline --test admin_upstreams custom_ca -- --nocapture
```

Expected: the response contains one failed key because the independent admin client rejects the private CA.

- [ ] **Step 3: Reuse `AppState` clients**

Expose one URL helper:

```rust
pub fn model_discovery_url(base_url: &str) -> String {
    crate::util::join_upstream_url(base_url, "/v1/models")
}
```

Use that exact URL to select a client:

```rust
let discovery_url = model_discovery_url(&payload.base_url);
let client = state.client_for_url(&discovery_url);
```

Apply the same pattern inside `build_model_probe_response` per upstream. Change `discover_batch_model_configuration` to accept `&AppState` and select the client from the batch payload URL. Preserve the existing request-level `admin_upstream_timeout_seconds` passed to model discovery.

- [ ] **Step 4: Run administrative tests and verify GREEN**

Run:

```bash
rtk cargo test --locked --offline --test admin_upstreams -- --nocapture
rtk cargo test --locked --offline --test admin_model_probe -- --nocapture
```

Expected: all tests pass, including the private-CA discovery case.

- [ ] **Step 5: Commit client unification**

```bash
rtk git add src/server/admin.rs src/state/model_discovery.rs tests/admin_upstreams.rs tests/upstream_ca.rs
rtk git commit -m "fix(admin): reuse trusted upstream clients"
```

### Task 3: Surface Safe Errors And Wire The Certificate Directory

**Files:**
- Modify: `frontend/src/api/admin.ts`
- Modify: `frontend/src/views/admin/Upstreams.vue`
- Modify: `frontend/tests/api/admin.spec.ts`
- Create: `certs/.gitignore`
- Create: `certs/README.md`
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `DEPLOYMENT.md`
- Modify: `tests/docker.rs`

- [ ] **Step 1: Write failing frontend and deployment tests**

Add a pure helper test:

```ts
expect(formatModelDiscoveryFailure({
  message: '所有 key 都无法获取模型列表',
  results: [
    { key_index: 0, error: 'upstream model discovery connection failed' },
    { key_index: 1, error: 'upstream model discovery returned status 403' }
  ]
})).toBe(
  '所有 key 都无法获取模型列表：Key #1: upstream model discovery connection failed；Key #2: upstream model discovery returned status 403'
)
```

Extend `tests/docker.rs` to require:

```rust
assert!(compose.contains("UPSTREAM_CA_CERT_PATH: ${UPSTREAM_CA_CERT_PATH:-}"));
assert!(compose.contains("./certs:/certs:ro"));
assert!(dotenv.contains("UPSTREAM_CA_CERT_PATH="));
```

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
rtk npm test -- --run tests/api/admin.spec.ts
rtk cargo test --locked --offline --test docker deployment_exposes_custom_upstream_ca -- --nocapture
```

Run the npm command from `frontend/`. Expected: frontend import or assertion fails and the deployment test fails because the new configuration is absent.

- [ ] **Step 3: Implement safe error formatting**

Export `formatModelDiscoveryFailure` from `frontend/src/api/admin.ts`. It must retain `result.message`, append only indexed `results[].error` strings, and fall back to `所有 Key 获取模型均失败` when no message exists. Replace the inline `ElMessage.error` expression in `Upstreams.vue` with this helper.

- [ ] **Step 4: Add the certificate directory and deployment wiring**

Create `certs/.gitignore`:

```gitignore
*
!.gitignore
!README.md
```

Document `.crt`/`.pem` CA-only files in `certs/README.md`. Add `UPSTREAM_CA_CERT_PATH=` to `.env.example`, interpolate it in Compose, and mount `./certs:/certs:ro`. Document file and directory modes, restart behavior, and the prohibition on private keys in `DEPLOYMENT.md`.

- [ ] **Step 5: Run frontend and deployment tests and verify GREEN**

Run:

```bash
rtk npm test -- --run tests/api/admin.spec.ts tests/views/admin-ui.spec.ts
rtk cargo test --locked --offline --test docker -- --nocapture
```

Run the npm command from `frontend/`. Expected: all selected tests pass.

- [ ] **Step 6: Commit deployment and UI changes**

```bash
rtk git add .env.example docker-compose.yml DEPLOYMENT.md certs frontend/src/api/admin.ts frontend/src/views/admin/Upstreams.vue frontend/tests/api/admin.spec.ts tests/docker.rs
rtk git commit -m "feat(deploy): wire internal CA directory"
```

### Task 4: Full Verification And Delivery

**Files:**
- Modify only files required by verification failures attributable to this feature.

- [ ] **Step 1: Format and lint**

Run:

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --locked --offline --all-targets --all-features -- -D warnings
```

Expected: both commands exit successfully with no warnings.

- [ ] **Step 2: Run backend and frontend suites**

Run:

```bash
rtk cargo test --locked --offline
rtk npm test -- --run
rtk npm run build
```

Run npm commands from `frontend/`. Expected: all Rust tests, all frontend tests, and the production build pass.

- [ ] **Step 3: Validate Compose and container startup**

Run:

```bash
rtk docker compose config --quiet
rtk scripts/build-package-image.sh --skip-npm-install --skip-frontend-build --skip-export
```

The packaging script builds the release binary and the `chat-responses-codex:latest`
runtime image from local artifacts. Start an isolated test container with a
generated CA directory, verify an invalid configured path prevents startup, and
verify the default blank configuration remains healthy. Do not modify the
existing production container or its Redis settings during this isolated check.

- [ ] **Step 4: Review the final diff**

Run:

```bash
rtk git diff origin/main...HEAD --check
rtk git status --short --branch
```

Expected: no whitespace errors and only intentional commits ahead of `origin/main`.

- [ ] **Step 5: Commit any verification-only corrections and push**

```bash
rtk git push origin main
```

Expected: `origin/main` advances to the verified implementation commit and the working tree is clean.
