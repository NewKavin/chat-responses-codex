# Model Groups & Model Access API

Model Groups and the model-access policy control which AI models each Portal
key / user may request.  The gateway validates the `model` parameter of every
`/v1/*` request against the resolved access policy at runtime (no restart
required).

## Concepts

- A **user** owns zero or more model groups (`portal_user_model_groups`) and
  may hold several keys (`downstreams` with `subject_kind=portal`).
- A **key's model access** is one of three explicit modes:
  - `inherit` — the union of the owner user's authorized groups (new keys
    default to this);
  - `group` — restricted to one specific group (`group_id`);
  - `deny` — always rejected, shown as `deny-all`.
  `deny-all`, `inherit`, and `group` are explicit modes; missing/empty/null
  selections are **never** reinterpreted as inherit (design decision 1).
- `["*"]` in `allowed_models` means **all models are allowed** (the seeded
  `all` group); it is never published as a literal model.
- The `basic` group is **protected**: it cannot be deleted.  Deleting any
  other group moves keys referencing it to `deny-all` (never to `inherit`).
- The user's granted-group union is the ceiling; a key's `group` mode narrows
  it further (`ceiling` intersected with `group`).  All, overlapping grants,
  and wildcard entries union/dedup correctly and never cross users.
- Holders without portal binding (direct-config downstreams) keep their
  legacy allowlist behavior (`All` when the list is empty or wildcard).
- Group changes apply immediately; there is no cache.  Policies live in
  `downstream_access_policies` with a monotonic `revision` used for
  conflict detection.

## Seeded Groups

| id | name | allowed_models |
|---|---|---|
| `basic` | Basic Models | `["gpt-3.5-turbo", "claude-3-haiku"]` |
| `premium` | Premium Models | `["gpt-4", "claude-opus-4-20250514"]` |
| `all` | All Models | `["*"]` |

## Admin Endpoints

Admin endpoints require the admin bearer token.

### List Model Groups

```http
GET /api/admin/model-groups
```

**Response:** 200

```json
{
  "groups": [
    {
      "id": "basic",
      "name": "Basic Models",
      "description": "Cost-effective models for development and testing",
      "allowed_models": ["gpt-3.5-turbo", "claude-3-haiku"],
      "created_at": 1725350400,
      "updated_at": 1725350400
    }
  ]
}
```

### Create Model Group

```http
POST /api/admin/model-groups
```

**Request:**

```json
{
  "id": "experimental",
  "name": "Experimental Models",
  "description": "Beta and experimental models",
  "allowed_models": ["gpt-4-turbo-preview", "claude-3-opus-20240229"]
}
```

**Response:** 201 Created (echoes the created group)

**Errors:**
- `400` — invalid id format (only lowercase letters, digits, hyphens) or empty `allowed_models`
- `409` — group id already exists

### Update Model Group

```http
PUT /api/admin/model-groups/{id}
```

**Request:**

```json
{
  "name": "Updated Name",
  "description": "Updated description",
  "allowed_models": ["model1", "model2"]
}
```

**Response:** 204 No Content

**Errors:**
- `400` — empty `allowed_models`
- `404` — group not found

### Delete Model Group

```http
DELETE /api/admin/model-groups/{id}
```

**Response:** 204 No Content

**Errors:**
- `403` — `cannot_delete_basic` (the `basic` group is protected)
- `404` — group not found

## Portal Endpoints

Portal endpoints require a valid portal session cookie.

### List Model Groups (read-only)

Portal users may read the group list to pick a group for their keys, but
cannot manage groups.

```http
GET /api/portal/model-groups
```

**Response:** 200 `{"groups": [...]}` (same shape as admin list)

### Create Key with Model Group

```http
POST /api/portal/keys
```

**Request:**

```json
{
  "downstream_id": "ds_abc123",
  "label": "My Key",
  "model_group_id": "premium"
}
```

`model_group_id` is optional; it defaults to `basic`.

**Response:** 201

```json
{
  "downstream_id": "ds_abc123",
  "model_group_id": "premium"
}
```

**Errors:**
- `404` — `model_group_not_found` (referenced group does not exist)

### List Keys (includes group info)

```http
GET /api/portal/keys
```

Each key includes `model_group_id` and `model_group_name`.

### Update Key's Model Group

```http
PUT /api/portal/keys/{downstream_id}/model-group
```

**Request:**

```json
{
  "model_group_id": "all"
}
```

**Response:** 204 No Content

**Errors:**
- `404` — `model_group_not_found` (target group does not exist)

## Gateway Enforcement

At request time the gateway resolves `resolved_model_access` for the key
(policy owner → user grant union → key mode/group), then checks the
requested model against the published catalog:

- key `deny` or policy lookup missing → `403` (`model_not_allowed`)
- user ceiling is empty or deny-all → `403`
- effective scope (`ceiling` ∩ group, or `All` for legacy direct holders)
  contains the model → forward (wire name from the route's catalog entry,
  never widened)
- stale or unroutable name → route-level `404`/`403`, never the owner's
  broader list

Capability failures keep the original error taxonomy
(`model_not_allowed` / `model_group_check_failed`).  If the permission lookup
fails with a database error, the request is rejected with `500` rather than
silently allowed.

## Frontend

- Admin console: `/admin/model-groups` — manage groups (create / edit /
  delete; `basic` cannot be deleted).
- Portal: Key Management shows each key's model access
  (inherit / group / deny), creates keys with `inherit` by default, and lets
  users change the mode/groups via the key card.
- Admin console: Portal Users — user grants (single + cross-user batch,
  add/remove/replace), per-key full configuration (incl. model access and
  `expires_at`), partial-failure readback, and the access-migration repair
  queue for pending records.
- Overview / Playground / Integration use the model-access contract for the
  effective model list (no quota-allowlist filtering of `/v1/models`, no
  literal `*` as a model).

## Model Access (2026-09-07 single-source model)

### Resolve the effective model scope for the current principal

```http
GET /api/portal/model-access?downstream_id=<optional>
```

`scope` is `user` without `downstream_id`, `key` with it.  An explicit
`downstream_id` must belong to the current principal; a foreign/unknown id
is rejected with `403 key_owner_mismatch` and never falls back to the
default key.  Response: `{ user_id, scope, downstream_id?, available_models[],
status: denied|no_routes|ready, reason?, source{user_group_ids, mode,
key_group_id}, model_access{mode, group_id?} }`.

### quota / models / overview / model-probe accept an optional scope

`GET /api/portal/quota?downstream_id=...`, `/api/portal/models?downstream_id=...`,
`/api/portal/overview?downstream_id=...`, `/api/portal/model-probe?downstream_id=...`.
Same ownership validation as above.  Array responses (models) keep their
array shape and echo the scope via the `X-Portal-Downstream-Id` response
header; object responses include a `downstream_id` field.

### Portal session (dual identity)

`GET /api/portal/session` now accepts both the OIDC cookie and the legacy
employee-id JWT / downstream secret, returning `auth_method`
(`cookie`/`legacy`), `login_downstream_id`, `default_downstream_id`, and
`has_keys`.  Portal-login JWTs carry `scope=portal` and are rejected by
admin auth.

### Keys: model access + full configuration

- `POST /api/portal/keys` — new key `model_access` defaults to `inherit`.
- `PUT /api/portal/keys/{id}/model-group` — accepts `model_access`
  (`{mode: inherit|group|deny, group_id?}`); the legacy `model_group_id`
  string is translated (`deny-all` → deny, other → group).
- `POST /api/admin/downstreams/{id}` + `PATCH /api/admin/downstreams/{id}` —
  `model_access`, `expires_at` (unix seconds; `null` clears), and every
  field of the full-config section with compatible null/absent semantics.
  The batch variant reports per-id results (`updated` / `failed`).
- `GET /api/admin/models?scope=exposed` — catalog entries including
  `multi_agent_version`; unrouted allowlist-only models are **not** published
  as fake models.

### Migration repair (admin)

```http
GET  /api/admin/portal/users/access-migration
POST /api/admin/portal/users/access-migration
     {"items":[{downstream_id, candidate_group_ids[], set_inherit,
                expected_revision, expected_fingerprint}]}
```

The preview includes a fingerprint over the user's grants, candidate group
content, and policy revision; `apply` re-checks it inside a transaction
(stale previews fail with `code=conflict`), locks the policy row, applies
the grants, sets the key to `inherit`, and marks the record resolved.
Conflict / orphan records are rejected, never auto-merged.

### Cross-user batch grants (admin)

```http
POST /api/admin/portal/users/batch-model-groups
  {"user_ids": [...], "op": "add|remove|replace", "model_group_ids": [...]}
```

One transaction per user; `replace` swaps the whole grant set (basic always
kept), removing `basic` is refused; results are reported per user in
`updated` / `failed`.

### Qualification (admin)

`POST /api/admin/upstreams/{id}/qualify` targets the group named by the
**policy** (group mode, non-builtin, referenced only by that key, and not
granted to any user); any other state returns a conflict without modifying
the group or other keys.
