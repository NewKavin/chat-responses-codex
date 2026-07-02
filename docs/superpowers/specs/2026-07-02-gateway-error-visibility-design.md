# Gateway Error Visibility Design

## Goal

Make gateway and upstream failures visible enough for downstream clients and the admin UI to diagnose common failures, while preserving the existing safety boundary that prevents prompt text, tool arguments, request bodies, and raw upstream echo messages from being exposed.

## Scope

This design covers gateway errors returned by `/v1/chat/completions`, `/v1/responses`, `/v1/messages`, `/v1/messages/count_tokens`, and usage-log entries shown on the admin logs page.

It does not change model routing, protocol translation semantics, upstream payload normalization, database schema, key management, or rate-limit accounting policy.

## Current Behavior

`GatewayError::into_response()` returns only:

```json
{
  "error": {
    "message": "..."
  }
}
```

The usage-log schema already has `error_message` and `error_category`, and the admin logs API already supports `error_category` and `error_categories` filtering. Stream failures already write categories such as `stream_idle_timeout` and `stream_upstream_body_decode_error`, but most non-stream failures write `None`.

The prior safety fix that replaces raw upstream error text with `safe_upstream_error_summary()` is correct and must remain. Upstream messages such as JSON parser echoes can include prompt/tool content and must not be copied into client responses or usage logs.

## Error Response Design

OpenAI-compatible endpoints return an OpenAI-style error envelope:

```json
{
  "error": {
    "message": "Daily token quota exceeded for this key.",
    "type": "gateway_quota_exceeded",
    "param": null,
    "code": "gateway_daily_token_quota_exceeded",
    "details": {
      "scope": "gateway",
      "quota": "daily_tokens",
      "retry_after_seconds": 3600
    }
  }
}
```

Claude-compatible endpoints return an Anthropic-style error envelope:

```json
{
  "type": "error",
  "error": {
    "type": "rate_limit_error",
    "message": "Daily token quota exceeded for this key.",
    "code": "gateway_daily_token_quota_exceeded",
    "details": {
      "scope": "gateway",
      "quota": "daily_tokens",
      "retry_after_seconds": 3600
    }
  }
}
```

`message` stays human-readable because downstream tools commonly display it directly. `code` and `details` are stable machine-readable additions. `details` is limited to gateway-owned facts: scope, category, retry-after seconds, quota kind, configured limit, current usage, and HTTP/upstream status classifications. It never includes raw upstream body text, request body snippets, prompt content, tool arguments, tool names, upstream API keys, downstream secrets, or arbitrary numbers parsed from upstream free-form text.

## Error Taxonomy

Gateway/downstream categories:

- `gateway_invalid_request`
- `gateway_auth_missing`
- `gateway_auth_invalid`
- `gateway_key_expired`
- `gateway_ip_not_allowed`
- `gateway_model_not_allowed`
- `gateway_per_minute_limit_exceeded`
- `gateway_request_quota_exceeded`
- `gateway_daily_token_quota_exceeded`
- `gateway_monthly_token_quota_exceeded`
- `gateway_concurrency_full`
- `gateway_no_routable_upstream`
- `gateway_response_history_invalid`

Upstream categories:

- `upstream_auth_error`
- `upstream_rate_limited`
- `upstream_concurrency_full`
- `upstream_protocol_unsupported`
- `upstream_context_limit`
- `upstream_request_rejected`
- `upstream_temporary_unavailable`
- `upstream_timeout`
- `upstream_network_error`
- `upstream_invalid_response`
- `upstream_empty_response`

Existing stream categories remain unchanged:

- `stream_client_cancelled`
- `stream_incomplete_close`
- `stream_interrupted`
- `stream_idle_timeout`
- `stream_max_duration`
- `stream_upstream_timeout`
- `stream_upstream_body_decode_error`
- `stream_upstream_read_error`

## Implementation Shape

Add structured metadata to `GatewayError` instead of deriving categories by parsing display strings. Downstream request reservation should return a structured rejection type so the gateway can distinguish per-minute limits, configured request quota, daily token quota, and monthly token quota without string matching.

All usage-log write sites that currently pass `None` for failed non-stream requests should pass `Some(error.error_category().to_string())`.

The admin logs API schema can stay as-is. The frontend only needs expanded category options, grouped quick filters, and status-code options for `499`, `503`, and `504`.

## Testing

Tests must prove:

- OpenAI-compatible errors keep `error.message` and add safe `type`, `code`, `param`, and `details`.
- Claude-compatible errors use Anthropic-style error envelopes.
- Downstream per-minute, request quota, daily token, and monthly token failures produce distinct codes and log categories.
- Upstream 429, upstream rejected request, context limit, timeout, invalid response, and no routable upstream produce stable safe categories.
- Raw upstream messages, prompt text, tool arguments, tool names, request secrets, and free-form numeric tokens are not exposed in client responses or usage logs.
- Admin logs filtering continues to work with the new category values.

