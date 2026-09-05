# Downstream Model Group - Manual Verification Guide

## Quick Start Verification

### 1. Start the Server

```bash
cd /home/kavin/projects/chat2Responses
cargo run --release
```

### 2. Access Admin Panel

Navigate to: `http://localhost:8080/admin`

Login with admin credentials (from your `.env` or config):
- Username: `admin`
- Password: (your configured password)

## Test Scenarios

### Scenario 1: Create Model Group

1. Go to **Model Groups** (`/admin/model-groups`)
2. Click **"Create Model Group"**
3. Fill in:
   - **Name**: `Basic Models`
   - **Description**: `Free tier models`
   - **Allowed Models**: Add `gpt-3.5-turbo`, `gpt-4o-mini`
4. Click **Save**
5. ✅ Verify: Group appears in list

### Scenario 2: Create Downstream with Model Group

1. Go to **Downstreams** (`/admin/downstreams`)
2. Click **"Create Downstream"**
3. Fill in basic info:
   - **ID**: `test-downstream-1`
   - **Name**: `Test Downstream 1`
4. In **"模型权限管理"** section:
   - Select **"使用模型分组（推荐）"**
   - Choose **"Basic Models"** from dropdown
   - ✅ Verify: Preview shows `gpt-3.5-turbo`, `gpt-4o-mini`
5. Click **Save**
6. ✅ Verify: Downstream shows "分组: Basic Models (2 个模型)"

### Scenario 3: Test Access Control

**3A. Request Allowed Model (Should Succeed)**

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer <downstream-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-3.5-turbo",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

✅ Expected: Request succeeds (200 OK or 502 if upstream not configured)

**3B. Request Blocked Model (Should Fail)**

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer <downstream-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

✅ Expected: Request rejected (403 Forbidden)
```json
{
  "error": {
    "message": "模型 'gpt-4' 不在下游 'test-downstream-1' 的允许列表中",
    "type": "forbidden"
  }
}
```

### Scenario 4: Update Model Group (Dynamic Propagation)

1. Go back to **Model Groups**
2. Edit **"Basic Models"**
3. Add `gpt-4` to allowed models
4. Click **Save**
5. **Without restarting server or editing downstream**, retry the curl from 3B
6. ✅ Verify: Request now succeeds (change propagated immediately)

### Scenario 5: Create Downstream with Manual Allowlist (Backward Compatibility)

1. Go to **Downstreams**
2. Click **"Create Downstream"**
3. Fill in basic info:
   - **ID**: `test-downstream-2`
   - **Name**: `Test Downstream 2`
4. In **"模型权限管理"** section:
   - Select **"手动配置模型列表"**
   - Add models: `claude-3-sonnet`, `claude-3-opus`
5. Click **Save**
6. ✅ Verify: Downstream shows model list, not group tag

### Scenario 6: Switch Between Modes

1. Edit an existing downstream
2. Switch from **"手动配置"** to **"使用模型分组"**
3. ✅ Verify: Manual model list is cleared
4. Select a model group
5. Save
6. Edit again and switch back to **"手动配置"**
7. ✅ Verify: Model group selection is cleared
8. Add manual models
9. Save

### Scenario 7: Wildcard Group (Allow All)

1. Create a model group with `*` in allowed models
2. Assign to a downstream
3. Test requests with various model names
4. ✅ Verify: All models allowed

### Scenario 8: Delete Model Group (Fallback)

1. Create a downstream with model group AND manual allowlist
2. Delete the model group
3. Make a request
4. ✅ Verify: 
   - Request uses fallback to manual allowlist
   - Server logs show warning about missing group

## Database Verification

```sql
-- Check schema
\d downstreams

-- View downstreams with model groups
SELECT id, name, model_group_id 
FROM downstreams 
WHERE model_group_id IS NOT NULL;

-- View model groups
SELECT id, name, allowed_models 
FROM model_groups;

-- Check foreign key constraint
SELECT
    tc.constraint_name,
    tc.table_name,
    kcu.column_name,
    ccu.table_name AS foreign_table_name,
    ccu.column_name AS foreign_column_name
FROM information_schema.table_constraints AS tc
JOIN information_schema.key_column_usage AS kcu
  ON tc.constraint_name = kcu.constraint_name
JOIN information_schema.constraint_column_usage AS ccu
  ON ccu.constraint_name = tc.constraint_name
WHERE tc.constraint_type = 'FOREIGN KEY' 
  AND tc.table_name = 'downstreams'
  AND kcu.column_name = 'model_group_id';
```

## Log Verification

When a downstream uses a model group, you should see logs like:

```
DEBUG downstream_id="test-downstream-1" model_group_id="basic-models" models_count=2 Using model group for downstream
```

When a model group is not found:

```
WARN downstream_id="test-downstream-1" model_group_id="deleted-group" error="Model group not found" Model group not found, falling back to model_allowlist
```

## Expected UI States

### Downstream List Table

| ID | Name | 模型权限 |
|----|------|---------|
| test-downstream-1 | Test Downstream 1 | 🔷 分组: Basic Models (2 个模型) |
| test-downstream-2 | Test Downstream 2 | gpt-3.5-turbo, gpt-4 |

### Downstream Edit Form

**Using Model Group:**
```
模型权限管理
  ◉ 使用模型分组（推荐）
  ○ 手动配置模型列表

选择模型分组
  [Dropdown: Basic Models (2 个模型) ▼]

  ┌─────────────────────────────────────┐
  │ 该分组允许的模型：                    │
  │ [gpt-3.5-turbo] [gpt-4o-mini]       │
  └─────────────────────────────────────┘

  ℹ️ 使用模型分组可以统一管理多个下游的模型权限。
     管理模型分组 →
```

**Manual Mode:**
```
模型权限管理
  ○ 使用模型分组（推荐）
  ◉ 手动配置模型列表

可用模型
  [Multi-select: gpt-3.5-turbo, gpt-4 ▼]

  ℹ️ 手动配置每个下游的模型列表。
     如需统一管理多个下游，推荐使用"模型分组"方式。
```

## Troubleshooting

### Issue: Model group dropdown is empty
**Solution**: Check that model groups exist in the database and the API endpoint returns them.

### Issue: Model group changes don't take effect
**Solution**: Check that the server reloads state properly. This should be automatic.

### Issue: Foreign key constraint violation
**Solution**: Check that the model group exists before assigning it to a downstream.

### Issue: Tests fail with "Database connection failed"
**Solution**: Ensure PostgreSQL is running and `DATABASE_URL` or `OIDC_TEST_DATABASE_URL` is set correctly.

## Performance Testing

Test with multiple downstreams using the same model group:

1. Create 1 model group
2. Create 10 downstreams, all using that group
3. Make concurrent requests to all downstreams
4. ✅ Verify: Performance is acceptable (no N+1 queries)
5. Check database query logs for efficiency

## Success Criteria

✅ All test scenarios pass
✅ No errors in server logs (warnings about missing groups are expected)
✅ UI is responsive and intuitive
✅ Model permissions enforced correctly
✅ Backward compatibility maintained
✅ Database constraints working
✅ TypeScript compilation clean
✅ All automated tests pass

## Automated Test Command

To run all tests:

```bash
# Backend tests
cargo test --test downstream_model_groups
cargo test --test admin_downstreams
cargo test --test admin_models
cargo test --test portal_api
cargo test --test portal_helpers

# Frontend type check
cd frontend && npm run type-check

# All tests
cargo test && cd frontend && npm run type-check
```

Expected output:
```
cargo test: 100 passed
vue-tsc: ✓
```
