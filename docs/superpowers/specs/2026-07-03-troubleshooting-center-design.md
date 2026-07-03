# Troubleshooting Center Design

## Summary

Add a unified troubleshooting center that helps downstream users verify client
configuration and gives administrators enough context to diagnose gateway,
upstream, quota, stream, and tool-call failures.

The first implementation should prioritize a guided downstream-user workflow:
select a client, select a model, run compatibility diagnostics, and receive a
clear pass/warn/fail result with actionable next steps. Administrator views
should reuse the same diagnostic output and add links into logs and active
request state.

This design does not change production gateway protocol conversion semantics.
Diagnostic runs must exercise the existing gateway endpoints through the same
authentication, routing, quota, model mapping, and upstream selection path that
real clients use.

## Goals

- Help Cline, Codex, opencode, Claude Code, Hermes, and generic SDK users
  confirm whether their key, base URL, model, and protocol choice are valid.
- Translate common gateway and upstream errors into concrete user-facing
  explanations, including subscription/token limits, request limits, upstream
  400 payload issues, 429 rate limits, context overflow, stream interruption,
  tool schema problems, and temporary upstream unavailability.
- Let administrators jump from a failed diagnostic to filtered logs and active
  long-running requests without manually reconstructing the context.
- Show long-task liveness using observed runtime state: active request age,
  selected upstream, protocol, last stream/event time, elapsed time, and latest
  error category when available.
- Keep diagnostics bounded, explicit, and opt-in so the feature does not create
  hidden traffic, long background jobs, or surprising quota consumption.

## Non-Goals

- Do not add an automatic 5-15 minute stress test in the first version.
- Do not bypass downstream authorization, quota checks, routing, or upstream
  compatibility rules during diagnostics.
- Do not add a new protocol dialect for a specific client.
- Do not mark models as universally compatible based only on their names.
- Do not introduce environment variables for the first version unless
  implementation reveals an unavoidable operational need.

## Entry Points

### Portal

Add a downstream-user page at `/portal/troubleshooting`.

The portal page is the primary entry point. It should be organized as a wizard:

1. Choose client profile.
2. Choose model.
3. Choose diagnostic scope.
4. Run diagnostics.
5. Review results and copy a support summary.

Client profiles:

- Cline
- Codex
- opencode
- Claude Code
- Hermes
- Generic OpenAI compatible
- Generic Anthropic compatible

### Admin

Add an admin page at `/admin/troubleshooting` and add a dedicated "排障中心"
item to the admin sidebar. The admin route should reuse shared troubleshooting
components and wrap them with administrator-only controls.

Admin-specific additions:

- Select downstream identity when running a diagnostic.
- Link failed diagnostic steps to filtered `/admin/logs` queries.
- Show active long-running requests across downstreams.
- Show selected upstream and upstream health context where the current API
  already exposes that information.

## Diagnostic Scope

The first version should implement "Agent compatibility" diagnostics.

Required checks:

1. **Authentication and model list**
   - Request `/v1/models` with the selected downstream key.
   - Confirm that the selected model is present in the returned model list.
   - Detect invalid key, missing authorization, disabled downstream, and empty
     model list.

2. **Chat Completions**
   - Call `/v1/chat/completions`.
   - Test both non-stream and stream where appropriate for the selected client.
   - Confirm HTTP status, valid response shape, and non-empty assistant output
     or recognized reasoning/thinking output.

3. **Responses**
   - Call `/v1/responses` for clients that use or can use Responses.
   - Confirm `response.created`, delta, and completion events for stream mode.
   - Treat long reasoning-only periods as a warning, not as an immediate empty
     response failure.

4. **Anthropic Messages**
   - Call `/v1/messages` for Claude Code, Anthropic-compatible clients, and
     clients that need Messages compatibility.
   - Confirm Anthropic SSE event shape for stream mode.

5. **Count Tokens**
   - Call `/v1/messages/count_tokens`.
   - Confirm HTTP 200 and numeric token count.
   - Surface unsupported or malformed responses as a compatibility warning.

6. **Tool schema compatibility**
   - Send a small function-tool payload representative of Cline/opencode style
     usage.
   - Include a tool schema where `parameters.required` is omitted to verify the
     gateway normalization path.
   - Confirm the request does not fail due to tool schema serialization.

7. **Stream liveness**
   - For stream diagnostics, record first-byte latency, first meaningful event
     latency, last event time, total duration, and whether the stream completed.
   - Timeouts should be explicit and short enough for an interactive page.
     First-version defaults: warn if no meaningful stream event arrives within
     20 seconds, and fail the diagnostic step after 90 seconds.

## Diagnostic Result Model

Each step should produce a structured result:

- `id`: stable step id.
- `label`: human-readable step name.
- `status`: `passed`, `warning`, `failed`, or `timeout`.
- `protocol`: `models`, `chat`, `responses`, `messages`, `count_tokens`, or
  `tools`.
- `client_profile`: selected client profile.
- `model`: selected model.
- `http_status`: downstream-visible status when available.
- `duration_ms`: total elapsed time.
- `summary`: concise user-facing result.
- `details`: sanitized diagnostic details.
- `error_category`: gateway error category when available.
- `suggestion`: recommended next action.
- `copy_summary`: text safe to send to an administrator or user.
- `log_filter`: optional filter payload for admin logs.

The result payload must not include upstream keys, downstream plaintext keys,
JWTs, full request bodies containing secrets, or raw headers containing
authorization material.

## Error Explanation

Reuse the existing frontend display helpers where possible, then extend them
for diagnostics.

Required user-facing categories:

- Invalid or missing downstream key.
- Downstream disabled or access denied.
- Selected model not exposed by `/v1/models`.
- Upstream 400 caused by request JSON, tool schema, or unsupported parameter.
- Upstream 429 or quota/rate limit.
- Downstream request limit.
- Daily or monthly token quota reached.
- Context length or input token overflow.
- Stream opened but produced no meaningful event before timeout.
- Stream interrupted after partial output.
- Tool calling unsupported by selected upstream/model.
- Upstream temporary unavailable or all upstream candidates failed.

The UI should distinguish:

- **Configuration errors**: user can fix base URL, key, or model.
- **Quota/rate errors**: user or admin must wait, increase limits, or change
  upstream.
- **Compatibility errors**: try another model, disable a feature, or adjust the
  client profile.
- **Upstream availability errors**: retry, inspect upstream health, or route to
  another upstream.

## Long-Task Observability

The first version should show long-running task state but should not run a
long-duration stress test automatically.

Add an active request view that answers:

- Which downstream is running the request?
- Which client/user-agent initiated it?
- Which model and protocol are active?
- Which upstream was selected?
- How long has it been running?
- When was the last stream/event observed?
- Has any error category been recorded?

The view should highlight:

- Running normally.
- Slow first token.
- No stream/event for 120 seconds after the request has already started
  producing or waiting for streamed output.
- Upstream returned an error.
- Client disconnected.

If the backend already tracks enough data for this view, reuse it. If not,
add a small in-memory runtime tracker that is updated at request start,
upstream dispatch, stream event, completion, and error. Persisting active
runtime state is not required for the first version.

## API Design

Add diagnostic APIs without changing `/v1/*` behavior.

Portal:

- `POST /api/portal/troubleshooting/run`
- `GET /api/portal/troubleshooting/active-requests`

Admin:

- `POST /api/admin/troubleshooting/run`
- `GET /api/admin/troubleshooting/active-requests`

Request shape:

```json
{
  "client_profile": "cline",
  "model": "GLM-5.1",
  "checks": ["models", "chat", "responses", "messages", "count_tokens", "tools"],
  "stream": true
}
```

Admin requests may include a downstream id:

```json
{
  "downstream_id": "test",
  "client_profile": "cline",
  "model": "GLM-5.1",
  "checks": ["models", "chat", "responses", "messages", "count_tokens", "tools"],
  "stream": true
}
```

Response shape:

```json
{
  "run_id": "diag_...",
  "client_profile": "cline",
  "model": "GLM-5.1",
  "status": "completed",
  "results": [
    {
      "id": "chat_stream",
      "label": "Chat Completions stream",
      "status": "passed",
      "protocol": "chat",
      "http_status": 200,
      "duration_ms": 1234,
      "summary": "Chat stream returned a valid SSE response.",
      "details": "First event after 320 ms; completed after 1234 ms.",
      "error_category": null,
      "suggestion": "This protocol is usable for the selected model.",
      "copy_summary": "Chat stream passed for GLM-5.1 through the gateway.",
      "log_filter": {
        "model": "GLM-5.1",
        "time_range": "1h"
      }
    }
  ]
}
```

The important contract is a structured per-step result and sanitized diagnostic
output. If implementation needs to extend this response, it should do so by
adding optional fields rather than changing the existing field meanings.

## Frontend UX

The portal wizard should be compact and task-focused:

- Client profile selector with short compatibility notes.
- Model selector sourced from `/v1/models` or the existing portal model list.
- Diagnostic checklist with sensible defaults per client profile.
- Run button with progress state per diagnostic step.
- Results timeline with status, duration, and recommendation.
- Copy buttons:
  - Copy client configuration.
  - Copy diagnostic summary for administrator.
  - Copy minimal curl reproduction when safe.

Client-specific copy:

- Cline: explain that Cline's "complex prompts" warning is a model capability
  note, not a gateway error. Actual failures should show HTTP status and
  gateway/upstream category.
- Codex: show Responses API configuration and model catalog reminders.
- Claude Code: show Anthropic-compatible base URL guidance and count token
  check.
- opencode and Hermes: show OpenAI-compatible base URL and model id guidance.

Admin UX:

- Filter by downstream, model, client profile, status, and error category.
- Open related logs with prefilled filters.
- Show active long-running requests in a table with "last event" age.
- Allow copying a sanitized support summary.

## Testing

Backend tests:

- Diagnostic run rejects missing or invalid authorization.
- Diagnostic run uses the same downstream key path as real requests.
- Model-list check detects missing selected model.
- Chat diagnostic handles success, upstream 400, upstream 429, and 503.
- Responses diagnostic recognizes stream lifecycle events.
- Claude Messages diagnostic recognizes Anthropic SSE events.
- Count-token diagnostic handles success and failure envelopes.
- Tool diagnostic covers omitted `parameters.required` in function schemas.
- Active request tracker records start, stream event, completion, and error.

Frontend tests:

- Wizard defaults per client profile.
- Result status rendering for passed, warning, failed, and timeout.
- Error explanation mapping for quota, rate limit, context, stream, tool, and
  upstream availability categories.
- Copy summary excludes secrets.
- Admin log deep-link filters are generated correctly.
- Active request status labels handle slow/no-event cases.

Deployment smoke:

- With the local deployment at `http://127.0.0.1:3000`, run diagnostics using
  the real downstream key against at least GLM-5.1.
- Confirm `/v1/models`, Chat Completions, Responses, Claude Messages,
  count_tokens, and tool schema checks return structured diagnostic results.
- Confirm portal and admin troubleshooting pages load in a headless browser.

## Rollout

1. Add shared diagnostic data structures and display helpers.
2. Add backend diagnostic runner and active request tracker.
3. Add portal troubleshooting page.
4. Add admin troubleshooting page and log deep links.
5. Add focused tests.
6. Run full frontend and backend verification.
7. Deploy to the local compose directory and run smoke tests with the real key.

## Decisions

- `/portal/troubleshooting` and `/admin/troubleshooting` should use shared
  components with separate route wrappers.
- Active request state is in-memory only for the first version.
- Diagnostic curl reproduction is admin-only in the first version because
  request bodies can include sensitive context.
- No new environment variables are part of this design.
