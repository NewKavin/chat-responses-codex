# Codex And OpenCode Portal Acceptance

Verification date: 2026-07-22

## Deployment

- The release binary was compiled on the host with
  `scripts/build-release-fast.sh --locked --offline`.
- The runtime image was built by copying that local binary only; no source was
  compiled in the image build.
- `scripts/deploy.sh --skip-build` recreated the gateway using that image.
- `/healthz` returned `ok` and gateway, PostgreSQL, and Redis were healthy.

## Portal-Generated Configuration

- Portal login succeeded and returned a live 33-model catalog.
- The generated Codex configuration and model catalog came from the portal
  generator, not from a hand-written client configuration.
- The catalog contained `MiniMax-M2.7`, `claude-sonnet-4-5-20250929`, and
  `gpt-5.6-sol`.
- Codex CLI `0.144.6` accepted the generated configuration with
  `--strict-config` and `doctor --summary` reported 17 checks passed and zero
  failures.
- The generated provider uses `stream_max_retries = 0`, so gateway terminal
  stream failures do not trigger client-side full-request reconnects.

## Installed Client Matrix

Both clients used isolated homes populated from the portal outputs. Text tasks
were substantive protocol questions; tool tasks required reading an absolute,
read-only marker file and the result was accepted only when the client emitted
the corresponding tool event and marker.

| Model | Codex `0.144.6` | OpenCode `1.17.18` | Result |
| --- | --- | --- | --- |
| `MiniMax-M2.7` | text passed; shell read tool passed | text passed; read tool passed | passed |
| `claude-sonnet-4-5-20250929` | text passed; shell read tool passed | text passed; read tool passed | passed |
| `gpt-5.6-sol` | text and tool requests returned the same terminal credential error | text and tool requests returned the same structured credential error | upstream credential failure, not client/protocol failure |

For `gpt-5.6-sol`, Codex received the terminal message
`all eligible upstream routes rejected their credentials`; OpenCode reported the
same message in its error event. The failure happened before tool execution and
was reproduced for both task types. Codex did not perform its default stream
reconnect loop.

## Automated Verification

- `cargo test --locked --offline -- --test-threads=1`: 1020 passed, 3 ignored
- `RUSTFLAGS=-Dwarnings cargo check --locked --offline --all-targets`: passed
- `npm test`: 174 passed across 27 files
- `npm run build`: passed
- `cargo test --locked --offline --test scripts`: 26 passed
- `cargo test --locked --offline --test templates`: 17 passed
- `cargo fmt --check` and `git diff --check`: passed
