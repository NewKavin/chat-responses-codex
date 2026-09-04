# Deployment Guide: Multi-Key Management

## Overview

This guide walks through deploying the multi-key management feature to production. The feature allows portal users to manage up to 10 API keys instead of a single key.

## Pre-Deployment Checklist

Before deploying, verify:

- [ ] All tests pass: `cargo test --workspace`
- [ ] Database backup completed
- [ ] Deployment window scheduled (low-traffic period recommended)
- [ ] Rollback plan reviewed
- [ ] Monitoring alerts configured
- [ ] Team notified of deployment

## Deployment Steps

### Step 1: Database Migrations

Run migrations in order. Both are idempotent and can run without downtime.

#### 1.1 Add Schema Columns

```bash
psql $DATABASE_URL -f migrations/2026-09-03-add-key-labels.sql
```

**Expected output**:
```
BEGIN
ALTER TABLE
ALTER TABLE
ALTER TABLE
CREATE INDEX
ALTER TABLE
CREATE INDEX
COMMIT
```

**What this does**:
- Adds `label` column (TEXT, nullable, max 100 chars)
- Adds `model_group_id` column (TEXT, default 'basic')
- Adds `created_at` column (TIMESTAMPTZ, default NOW())
- Creates index on `user_id` for faster queries
- Creates index on `response_history(downstream_key_id, created_at DESC)` for usage stats

**Verification**:
```sql
\d portal_user_downstreams
```

You should see the new columns:
```
 label           | text                        |
 model_group_id  | text                        | default 'basic'::text
 created_at      | timestamp with time zone    | default now()
```

#### 1.2 Migrate Existing Keys to Default

```bash
psql $DATABASE_URL -f migrations/2026-09-03-migrate-existing-keys-to-default.sql
```

**Expected output**:
```
BEGIN
UPDATE N  -- N = number of single-key users
UPDATE M  -- M = number of multi-key users without default
DO
COMMIT
```

**What this does**:
- Sets `is_default = true` for users with exactly one key
- For users with multiple keys but no default, marks the oldest key as default
- Validates that every user has exactly one default key

**Verification**:
```sql
-- Check that every user has exactly one default
SELECT user_id, COUNT(*) as total_keys, SUM(CASE WHEN is_default THEN 1 ELSE 0 END) as default_keys
FROM portal_user_downstreams
GROUP BY user_id
HAVING SUM(CASE WHEN is_default THEN 1 ELSE 0 END) != 1;
```

Expected: **No rows** (empty result means all users have exactly one default).

If this query returns rows, DO NOT PROCEED. Investigate and fix data inconsistencies first.

---

### Step 2: Deploy Backend

#### 2.1 Build and Test

```bash
# Build release binary
cargo build --release

# Run tests one more time
cargo test --workspace
```

Expected: All tests pass.

#### 2.2 Deploy Binary

**For Docker Compose deployments**:
```bash
docker compose build
docker compose up -d
```

**For standalone deployments**:
```bash
# Stop current service
systemctl stop chat-responses-codex

# Replace binary
cp target/release/chat-responses-codex /usr/local/bin/

# Start service
systemctl start chat-responses-codex
```

#### 2.3 Verify Backend Health

```bash
# Check service is running
curl http://localhost:3001/health

# Check logs
docker compose logs -f gateway
# or
journalctl -u chat-responses-codex -f
```

Expected: No errors, service responds with 200 OK.

---

### Step 3: Deploy Frontend

#### 3.1 Build Frontend

```bash
cd frontend
npm install
npm run build
```

Expected: Build completes without errors, creates `dist/` directory.

#### 3.2 Deploy Frontend Assets

**For Docker Compose** (frontend is built into the image):
```bash
# Already deployed in Step 2.2
docker compose logs frontend
```

**For standalone deployments** (serving via nginx/caddy):
```bash
# Copy built assets to web root
cp -r frontend/dist/* /var/www/portal/

# Reload web server
systemctl reload nginx
```

#### 3.3 Verify Frontend

Open browser and navigate to:
```
https://your-gateway.example.com/keys
```

Expected: Page loads, shows keys management interface.

---

### Step 4: Verification

Run these verification steps to confirm the deployment is working correctly.

#### 4.1 List Keys API

```bash
# Get session cookie (login first)
SESSION_ID="your_session_id_here"

# List keys
curl -H "Cookie: portal_session_id=$SESSION_ID" \
     http://localhost:3001/portal/api/keys
```

Expected response:
```json
[
  {
    "downstream_id": "ds_...",
    "is_default": true,
    "label": "Default Key",
    "model_group_id": "basic",
    "created_at": 1725360000,
    "usage_count": 0
  }
]
```

#### 4.2 Create Key API

```bash
curl -X POST \
     -H "Cookie: portal_session_id=$SESSION_ID" \
     -H "Content-Type: application/json" \
     -d '{"label": "Test Key", "model_group_id": "basic"}' \
     http://localhost:3001/portal/api/keys
```

Expected: 201 Created with new key details.

#### 4.3 Update Label API

```bash
curl -X PATCH \
     -H "Cookie: portal_session_id=$SESSION_ID" \
     -H "Content-Type: application/json" \
     -d '{"label": "Updated Label"}' \
     http://localhost:3001/portal/api/keys/ds_your_key_id
```

Expected: 204 No Content.

#### 4.4 Set Default API

```bash
curl -X POST \
     -H "Cookie: portal_session_id=$SESSION_ID" \
     http://localhost:3001/portal/api/keys/ds_your_key_id/set-default
```

Expected: 204 No Content.

#### 4.5 Delete Key API

```bash
curl -X DELETE \
     -H "Cookie: portal_session_id=$SESSION_ID" \
     http://localhost:3001/portal/api/keys/ds_non_default_key_id
```

Expected: 204 No Content (only non-default keys can be deleted).

#### 4.6 Verify Default Key Protection

```bash
# Try to delete default key (should fail)
curl -X DELETE \
     -H "Cookie: portal_session_id=$SESSION_ID" \
     http://localhost:3001/portal/api/keys/ds_default_key_id
```

Expected: 400 Bad Request with error message "Cannot delete default key".

#### 4.7 Verify 10-Key Limit

Create keys until you have 10, then try to create an 11th:

```bash
curl -X POST \
     -H "Cookie: portal_session_id=$SESSION_ID" \
     -H "Content-Type: application/json" \
     -d '{"label": "11th Key"}' \
     http://localhost:3001/portal/api/keys
```

Expected: 400 Bad Request with error message "Maximum 10 keys per user".

#### 4.8 Frontend Verification

Manual testing in browser:

1. Navigate to `/keys` page
2. Verify keys list displays correctly
3. Create a new key (check modal opens, form works)
4. Edit a key label (check modal opens, update works)
5. Set a key as default (check badge moves)
6. Delete a non-default key (check confirmation dialog)
7. Try to delete default key (check error message)
8. Create keys until limit (check create button disables at 10)

---

### Step 5: Post-Deployment Monitoring

#### 5.1 Database Queries

Monitor key creation and usage:

```sql
-- Count users by number of keys
SELECT 
  keys_per_user,
  COUNT(*) as user_count
FROM (
  SELECT user_id, COUNT(*) as keys_per_user
  FROM portal_user_downstreams
  GROUP BY user_id
) subq
GROUP BY keys_per_user
ORDER BY keys_per_user;

-- Find most active keys
SELECT 
  d.downstream_id,
  d.label,
  d.user_id,
  COUNT(r.id) as request_count
FROM portal_user_downstreams d
LEFT JOIN response_history r ON d.downstream_id = r.downstream_key_id
WHERE r.created_at > NOW() - INTERVAL '24 hours'
GROUP BY d.downstream_id, d.label, d.user_id
ORDER BY request_count DESC
LIMIT 20;

-- Check for users without default keys (should be empty)
SELECT user_id, COUNT(*) as total_keys
FROM portal_user_downstreams
GROUP BY user_id
HAVING SUM(CASE WHEN is_default THEN 1 ELSE 0 END) != 1;
```

#### 5.2 Application Logs

Watch for errors related to key management:

```bash
# Docker Compose
docker compose logs -f gateway | grep -i "key\|downstream"

# Systemd
journalctl -u chat-responses-codex -f | grep -i "key\|downstream"
```

Look for:
- Errors in key creation/update/delete operations
- 400/404 errors on key endpoints
- SQL constraint violations
- Default key inconsistencies

#### 5.3 Performance Metrics

Monitor API endpoint latency:

- `GET /portal/api/keys` - Should be < 100ms
- `POST /portal/api/keys` - Should be < 50ms
- `PATCH /portal/api/keys/:id` - Should be < 50ms
- `DELETE /portal/api/keys/:id` - Should be < 50ms
- `POST /portal/api/keys/:id/set-default` - Should be < 100ms (transaction)

---

## Rollback Procedure

If critical issues are detected, follow this rollback procedure:

### Rollback Step 1: Revert Backend

**Docker Compose**:
```bash
# Stop current version
docker compose down

# Checkout previous version
git checkout <previous-commit>

# Rebuild and start
docker compose up -d --build
```

**Standalone**:
```bash
# Stop service
systemctl stop chat-responses-codex

# Restore previous binary
cp /backup/chat-responses-codex /usr/local/bin/

# Start service
systemctl start chat-responses-codex
```

### Rollback Step 2: Revert Frontend

```bash
cd frontend
git checkout <previous-commit>
npm run build
# Deploy as in Step 3.2
```

### Rollback Step 3: Revert Database (CAUTION)

**Only if data corruption detected**:

```sql
BEGIN;

-- Remove new columns (data will be lost)
ALTER TABLE portal_user_downstreams DROP COLUMN IF EXISTS label;
ALTER TABLE portal_user_downstreams DROP COLUMN IF EXISTS model_group_id;
ALTER TABLE portal_user_downstreams DROP COLUMN IF EXISTS created_at;

-- Drop new indexes
DROP INDEX IF EXISTS idx_portal_user_downstreams_user_id;
DROP INDEX IF EXISTS idx_response_history_downstream_created;

COMMIT;
```

**WARNING**: Rolling back the database will delete all labels, model groups, and creation timestamps. Only do this if absolutely necessary.

**Better approach**: Keep the schema and fix the data:

```sql
-- Reset all users to single default key
BEGIN;

-- For each user, keep only their oldest key as default
WITH users_with_multiple_keys AS (
  SELECT user_id
  FROM portal_user_downstreams
  GROUP BY user_id
  HAVING COUNT(*) > 1
),
keys_to_keep AS (
  SELECT DISTINCT ON (user_id) downstream_id, user_id
  FROM portal_user_downstreams
  WHERE user_id IN (SELECT user_id FROM users_with_multiple_keys)
  ORDER BY user_id, created_at ASC, downstream_id ASC
)
DELETE FROM portal_user_downstreams
WHERE user_id IN (SELECT user_id FROM users_with_multiple_keys)
  AND downstream_id NOT IN (SELECT downstream_id FROM keys_to_keep);

-- Ensure all remaining keys are default
UPDATE portal_user_downstreams SET is_default = true;

COMMIT;
```

### Rollback Verification

After rollback:

1. Verify service is running: `curl http://localhost:3001/health`
2. Check logs for errors
3. Test core functionality (login, API requests)
4. Notify team of rollback

---

## Common Issues and Solutions

### Issue: Migration fails with "label_max_length constraint violated"

**Cause**: Existing data has labels exceeding 100 characters.

**Solution**:
```sql
-- Find offending rows
SELECT downstream_id, user_id, length(label) as label_length
FROM portal_user_downstreams
WHERE label IS NOT NULL AND length(label) > 100;

-- Truncate labels
UPDATE portal_user_downstreams
SET label = substring(label, 1, 100)
WHERE label IS NOT NULL AND length(label) > 100;

-- Retry migration
```

### Issue: User has no default key after migration

**Cause**: Migration script logic issue or concurrent updates.

**Solution**:
```sql
-- Fix specific user
UPDATE portal_user_downstreams
SET is_default = true
WHERE user_id = 'affected_user_id'
  AND downstream_id = (
    SELECT downstream_id
    FROM portal_user_downstreams
    WHERE user_id = 'affected_user_id'
    ORDER BY created_at ASC
    LIMIT 1
  );
```

### Issue: 500 errors on key creation

**Check**:
1. Database connection: `psql $DATABASE_URL -c "SELECT 1"`
2. Pool exhaustion: Check `POSTGRES_POOL_MAX_SIZE`
3. Disk space: `df -h`
4. Logs: Look for SQL errors or panics

### Issue: Frontend not loading keys

**Check**:
1. API endpoint: `curl http://localhost:3001/portal/api/keys` (with session cookie)
2. CORS headers (if frontend on different domain)
3. Browser console for JS errors
4. Network tab for failed requests

---

## Success Criteria

Deployment is successful when:

- [ ] All migrations completed without errors
- [ ] Backend service running and responding to health checks
- [ ] Frontend page loads and displays keys
- [ ] All 5 API endpoints verified working
- [ ] Default key protection working (cannot delete default)
- [ ] 10-key limit enforced
- [ ] No errors in application logs
- [ ] No users with 0 or 2+ default keys
- [ ] Performance metrics within acceptable ranges
- [ ] Team confirmed functionality

---

## Support and Troubleshooting

For issues during deployment:

1. Check application logs first
2. Verify database state with SQL queries
3. Test API endpoints with curl
4. Review this guide's "Common Issues" section
5. Rollback if critical issues cannot be resolved quickly

Post-deployment questions or issues: Contact the platform team or file an issue in the repository.
