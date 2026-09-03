# Multi-Key Management Feature

## Overview

Portal users can now manage multiple downstream API keys instead of a single key. This enables:

- **Separate keys for different environments** (development, staging, production)
- **Easy key rotation** without service interruption
- **Custom labels** for organizational purposes
- **Usage tracking** per key
- **Flexible key lifecycle** management

## Key Properties

Each key has the following attributes:

- **downstream_id**: Unique identifier (format: `ds_<uuid>`)
- **label**: Custom name, max 100 characters (optional, defaults to "Default Key")
- **model_group_id**: Associated model group (default: "basic")
- **is_default**: Boolean flag indicating the default key for the user
- **created_at**: Unix timestamp when the key was created
- **usage_count**: Number of API requests made with this key

## Key Constraints

- **Maximum 10 keys per user** - Creating an 11th key returns a 400 error
- **At least 1 key required** - Users cannot delete their last key
- **Exactly 1 default key** - One key must always be marked as default
- **Label length limit** - Labels cannot exceed 100 characters

## API Endpoints

### List Keys

```
GET /portal/api/keys
```

Returns all keys for the authenticated user, sorted by default status first, then by creation time (newest first).

**Authentication**: Requires `portal_session_id` cookie.

**Response** (200 OK):
```json
[
  {
    "downstream_id": "ds_abc123-...",
    "is_default": true,
    "label": "Production Key",
    "model_group_id": "basic",
    "created_at": 1725360000,
    "usage_count": 42
  },
  {
    "downstream_id": "ds_xyz789-...",
    "is_default": false,
    "label": "Development Key",
    "model_group_id": "basic",
    "created_at": 1725360100,
    "usage_count": 7
  }
]
```

**Error Responses**:
- `401 Unauthorized` - Not logged in

---

### Create Key

```
POST /portal/api/keys
Content-Type: application/json

{
  "label": "My New Key",
  "model_group_id": "basic"
}
```

Creates a new key for the authenticated user. The first key is automatically set as default.

**Request Body**:
- `label` (optional): Custom name for the key, max 100 characters
- `model_group_id` (optional): Model group identifier, defaults to "basic"

**Response** (201 Created):
```json
{
  "downstream_id": "ds_abc123-...",
  "is_default": false,
  "label": "My New Key",
  "model_group_id": "basic",
  "created_at": 1725360200,
  "usage_count": 0
}
```

**Error Responses**:
- `400 Bad Request` - Label exceeds 100 characters
- `400 Bad Request` - Maximum 10 keys per user
- `401 Unauthorized` - Not logged in

---

### Update Key Label

```
PATCH /portal/api/keys/:downstream_id
Content-Type: application/json

{
  "label": "Updated Key Name"
}
```

Updates the label of an existing key.

**Path Parameters**:
- `downstream_id`: The ID of the key to update

**Request Body**:
- `label` (optional): New label, max 100 characters. Empty string sets label to NULL.

**Response** (204 No Content):
Empty body on success.

**Error Responses**:
- `400 Bad Request` - Label exceeds 100 characters
- `404 Not Found` - Key not found or not owned by user
- `401 Unauthorized` - Not logged in

---

### Delete Key

```
DELETE /portal/api/keys/:downstream_id
```

Deletes a non-default key.

**Path Parameters**:
- `downstream_id`: The ID of the key to delete

**Response** (204 No Content):
Empty body on success.

**Error Responses**:
- `400 Bad Request` - Cannot delete default key
- `404 Not Found` - Key not found or not owned by user
- `401 Unauthorized` - Not logged in

**Important**: To delete a default key, first set another key as default, then delete it.

---

### Set Default Key

```
POST /portal/api/keys/:downstream_id/set-default
```

Sets the specified key as the default for the user. This operation is atomic: the previous default is cleared and the new default is set in a single transaction.

**Path Parameters**:
- `downstream_id`: The ID of the key to set as default

**Response** (204 No Content):
Empty body on success.

**Error Responses**:
- `404 Not Found` - Key not found or not owned by user
- `401 Unauthorized` - Not logged in

---

## User Workflows

### Creating Your First Key

When a user first creates a key, it is automatically marked as default:

1. User creates key via `POST /portal/api/keys`
2. Gateway generates a unique `downstream_id`
3. Key is marked as `is_default = true`
4. User can immediately use this key for API requests

### Adding Additional Keys

To add more keys for different environments:

1. User creates additional keys via `POST /portal/api/keys`
2. New keys are marked as `is_default = false`
3. User can set custom labels to distinguish keys (e.g., "Staging", "Production")
4. Each key tracks its own usage independently

### Rotating Keys

To rotate a key without downtime:

1. Create a new key with `POST /portal/api/keys`
2. Update client applications to use the new key
3. Set the new key as default with `POST /portal/api/keys/:new_id/set-default`
4. Once old key is no longer in use, delete it with `DELETE /portal/api/keys/:old_id`

### Key Organization

Best practices for key management:

- **Use labels** - Give each key a descriptive name (e.g., "Production API", "CI/CD Pipeline")
- **Set default wisely** - The default key is used by the gateway when routing requests
- **Monitor usage** - Check `usage_count` to identify unused keys
- **Rotate regularly** - Create new keys and delete old ones periodically
- **Separate environments** - Use different keys for dev, staging, and production

## Technical Implementation

### Database Schema

Keys are stored in the `portal_user_downstreams` table:

```sql
CREATE TABLE portal_user_downstreams (
  user_id TEXT NOT NULL,
  downstream_id TEXT PRIMARY KEY,
  is_default BOOLEAN NOT NULL DEFAULT false,
  label TEXT CHECK (label IS NULL OR char_length(label) <= 100),
  model_group_id TEXT DEFAULT 'basic',
  created_at TIMESTAMPTZ DEFAULT NOW()
);
```

### Default Key Selection

When the gateway needs to route a request for a user, it queries for the default key:

```sql
SELECT downstream_id 
FROM portal_user_downstreams 
WHERE user_id = $1 AND is_default = true
```

### Usage Tracking

The `usage_count` field aggregates from the `response_history` table:

```sql
SELECT COUNT(r.id) AS usage_count
FROM portal_user_downstreams d
LEFT JOIN response_history r ON d.downstream_id = r.downstream_key_id
WHERE d.user_id = $1
GROUP BY d.downstream_id
```

### Atomic Default Key Update

Setting a new default key is an atomic operation:

```sql
BEGIN;

-- Clear all defaults for this user
UPDATE portal_user_downstreams 
SET is_default = false 
WHERE user_id = $1;

-- Set new default
UPDATE portal_user_downstreams 
SET is_default = true 
WHERE user_id = $1 AND downstream_id = $2;

COMMIT;
```

## Migration from Single Key

Existing users with a single key will see their key automatically marked as default. The migration script `2026-09-03-migrate-existing-keys-to-default.sql` ensures:

1. Users with one key → that key becomes default
2. Users with multiple keys but no default → oldest key becomes default
3. Users with an existing default → no change

This migration is idempotent and can be run safely multiple times.

## Frontend Integration

The frontend keys management page (`/keys`) provides:

- **Visual key list** - Cards showing each key's details
- **Default badge** - Clear indication of which key is default
- **Create button** - Disabled when user has 10 keys
- **Edit label** - Modal dialog for updating labels
- **Set default** - Button on non-default keys
- **Delete button** - Only shown for non-default keys
- **Usage display** - Shows request count per key

## Security Considerations

- **Ownership validation** - All operations verify the key belongs to the requesting user
- **Session authentication** - All endpoints require a valid portal session
- **No key exposure** - The full key value is never returned in API responses
- **Audit trail** - All operations are logged with user_id and downstream_id

## Performance Characteristics

- **List keys** - Single query with LEFT JOIN for usage counts (no N+1)
- **Create key** - Two queries (count check + insert)
- **Update label** - Two queries (ownership check + update)
- **Delete key** - Two queries (ownership/default check + delete)
- **Set default** - Transaction with two updates (atomic)

All queries are indexed appropriately:
- `idx_portal_user_downstreams_user_id` on `user_id`
- `idx_response_history_downstream_created` on `(downstream_key_id, created_at DESC)`
