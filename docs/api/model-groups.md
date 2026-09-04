# Model Groups API

Model Groups let administrators control which AI models each Portal key may
request. Every Portal key is bound to exactly one model group; the gateway
validates the `model` parameter of each `/v1/*` request against the group's
`allowed_models` list at runtime (no restart required).

## Concepts

- A **model group** has an `id` (lowercase letters, digits, hyphens), a
  display `name`, an optional `description`, and an `allowed_models` array.
- `["*"]` in `allowed_models` means **all models are allowed** (the seeded
  `all` group).
- The `basic` group is **protected**: it cannot be deleted, and deleting any
  other group resets dependent keys back to `basic` (`ON DELETE SET DEFAULT`).
- Keys created without an explicit group default to `basic`.
- Keys that have **no portal binding** (direct-config downstreams) are **not
  restricted** — validation is skipped for backward compatibility.
- Group changes apply immediately; there is no cache.

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

At request time the gateway resolves the downstream key to its portal
binding, then checks the requested `model` against the group's
`allowed_models`:

- `allowed_models` contains `*` → allow
- `allowed_models` contains the requested model → allow
- empty list (no portal binding) → skip validation (backward compatible)
- otherwise → `403` with code `model_not_allowed`

If the permission lookup fails (database error), the request is rejected
with `500` (`model_group_check_failed`) rather than silently allowed.

## Frontend

- Admin console: `/admin/model-groups` — manage groups (create / edit /
  delete; `basic` cannot be deleted).
- Portal: Key Management page shows each key's group name, lets users pick a
  group when creating a key, and change a key's group via the card.
