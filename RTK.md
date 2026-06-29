# RTK - Rust Token Killer (Codex CLI)

**Usage**: Token-optimized CLI proxy for shell commands.

## Rule (MANDATORY)

**Always prefix shell commands with `rtk`. No exceptions.**

This applies to EVERY command, including but not limited to:
- `rtk docker exec ... psql ...`
- `rtk docker logs ...`
- `rtk curl ...`
- `rtk cat / grep / sed / awk / find / ls`
- `rtk python3 ...`
- `rtk ps / ss / lsof`
- `rtk git / cargo / npm / bun`

If a command is not prefixed with `rtk`, it is a violation.

## Examples

```bash
rtk git status
rtk cargo test
rtk npm run build
rtk pytest -q
rtk docker exec chat-responses-codex-postgres psql -U chat_responses_codex -d chat_responses_codex -c "SELECT ..."
rtk curl -s -m 30 http://127.0.0.1:3000/v1/models -H "Authorization: Bearer ..."
rtk /home/kavin/.local/bin/codebase-memory-mcp cli ...
```

## Meta Commands

```bash
rtk gain            # Token savings analytics
rtk gain --history  # Recent command savings history
rtk proxy <cmd>     # Run raw command without filtering
```

## Verification

```bash
rtk --version
rtk gain
which rtk
```

## Self-Check Before Every Command

Before running any shell command, ask: "Does it start with `rtk `?"
If no -> add the prefix. If unsure -> use `rtk proxy <cmd>`.
