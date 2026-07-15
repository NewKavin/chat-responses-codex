# Validated Downstream Secret Fast Path Design

## Goal

Keep the persisted Argon2 or legacy hash authoritative while removing Argon2
verification from the normal gateway request path when a matching plaintext key
is already stored for administrative retrieval.

## Invariants

- A stored plaintext key is trusted only after it verifies against the stored
  hash during state construction or configuration mutation.
- A mismatched or malformed hash clears the in-memory and next-persisted
  plaintext fast path. It never authenticates a request.
- A validated plaintext key is compared to a request key in constant time.
- Records without a validated plaintext key retain the existing Argon2 or
  legacy-hash verification behavior.
- Downstream creation and rotation update hash and plaintext together, then
  pass through the same validation boundary before publication.
- Portal, standard model listing, Codex catalog listing, and inference dispatch
  use the same normalized authentication path.

## State Boundary

`AppState` validates downstream credential pairs in every constructor and in
`mutate_persisted_state` before persistence and publication. Authentication
helpers outside `AppState` do not accept an unvalidated plaintext value.

Invalid stored pairs are fail-closed: the plaintext is removed, a bounded log
records only the downstream ID, and hash verification remains available. No
secret, hash, or plaintext prefix is logged.

## Performance

Valid current records perform one Argon2 verification when state is loaded or
the downstream is changed. Normal requests perform a constant-time comparison.
The release first-event benchmark must keep gateway-added P95 below 50 ms, and
the troubleshooting latency test must observe the delayed upstream event rather
than authentication cost.

## Verification

- Unit coverage for valid and invalid Argon2 and legacy pairs.
- AppState coverage proving a matching stale plaintext cannot override a
  mismatched or malformed hash.
- Authentication coverage for inference, model listing, and rotation.
- `cargo test --locked --test troubleshooting compatibility_matrix_records_first_meaningful_event_latency -- --nocapture`.
- `cargo test --release --test load load_gateway_first_meaningful_event -- --ignored --exact --nocapture`.

