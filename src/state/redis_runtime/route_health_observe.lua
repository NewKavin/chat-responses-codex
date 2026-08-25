local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local action = ARGV[1]
local kind = ARGV[2]
local class = ARGV[3]
local explicit_retry_ms = tonumber(ARGV[4])
local streak_reset_ms = tonumber(ARGV[5])
local ttl_seconds = tonumber(ARGV[6])
local global_capacity = tonumber(ARGV[7])
local upstream_capacity = tonumber(ARGV[8])
local exact_retry = ARGV[13] == '1'
local failure_status = ARGV[14]
local schedule_count = tonumber(ARGV[15])
-- schedule values live at ARGV[16..15+schedule_count]; max_step is appended
-- after them. Guarded: the clear/reconcile actions pass fewer args.
local max_step = schedule_count and tonumber(ARGV[16 + schedule_count]) or nil

local function prune_index(index_key)
  local members = redis.call('ZRANGE', index_key, 0, -1)
  for _, member in ipairs(members) do
    if redis.call('EXISTS', member) == 0 then
      redis.call('ZREM', index_key, member)
    end
  end
end

local function evict_one(index_key)
  local members = redis.call('ZRANGE', index_key, 0, -1)
  for _, member in ipairs(members) do
    local active = redis.call('HGET', member, 'half_open_lease')
    if not active or active == '' then
      local upstream_index = redis.call('HGET', member, 'upstream_index_key')
      redis.call('DEL', member)
      redis.call('ZREM', KEYS[3], member)
      if upstream_index and upstream_index ~= '' then
        redis.call('ZREM', upstream_index, member)
      else
        redis.call('ZREM', KEYS[2], member)
      end
      return true
    end
  end
  return false
end

if action == 'reconcile' then
  local removed = 0
  for index = 2, #KEYS, 2 do
    local state_key = KEYS[index]
    local upstream_index = KEYS[index + 1]
    local active = redis.call('HGET', state_key, 'half_open_lease')
    local expires_at = tonumber(
      redis.call('HGET', state_key, 'half_open_expires_at_ms') or '0'
    )
    if active and active ~= '' and expires_at > now_ms then
      redis.call('HSET', state_key, 'reconcile_pending', '1')
    else
      redis.call('DEL', state_key)
      redis.call('ZREM', KEYS[1], state_key)
      redis.call('ZREM', upstream_index, state_key)
      removed = removed + 1
    end
  end
  return removed
end

if action == 'clear' then
  redis.call('DEL', KEYS[1])
  redis.call('ZREM', KEYS[2], KEYS[1])
  redis.call('ZREM', KEYS[3], KEYS[1])
  return 1
end

if redis.call('EXISTS', KEYS[1]) == 0 then
  prune_index(KEYS[2])
  prune_index(KEYS[3])
  if redis.call('ZCARD', KEYS[2]) >= upstream_capacity and not evict_one(KEYS[2]) then
    return 0
  end
  if redis.call('ZCARD', KEYS[3]) >= global_capacity and not evict_one(KEYS[3]) then
    return 0
  end
end

local previous_class = redis.call('HGET', KEYS[1], 'failure_class')
local previous_at = tonumber(redis.call('HGET', KEYS[1], 'last_failure_ms') or '0')
local previous_count = tonumber(redis.call('HGET', KEYS[1], 'failure_count') or '0')
local step = 1
if previous_class == class and now_ms - previous_at <= streak_reset_ms then
  step = math.max(1, previous_count + 1)
  -- T1.3: non-half-open failures are capped at the configured max step so
  -- the local backoff arm can never outrun the retry wait budget. Mirrors
  -- failure_step in route_health.rs.
  if max_step and max_step >= 1 then
    step = math.min(step, max_step)
  end
end

local cooldown_ms = 0
if schedule_count > 0 then
  local schedule_index = math.min(step, schedule_count)
  cooldown_ms = tonumber(ARGV[15 + schedule_index])
end
if explicit_retry_ms >= 0 then
  if exact_retry then
    cooldown_ms = explicit_retry_ms
  else
    cooldown_ms = math.max(cooldown_ms, explicit_retry_ms)
  end
end

local generation = redis.call('INCR', KEYS[4])
redis.call('HSET', KEYS[1],
  'failure_count', tostring(step),
  'failure_class', class,
  'last_failure_ms', tostring(now_ms),
  'state_generation', tostring(generation),
  'last_access_ms', tostring(now_ms),
  'upstream_id', ARGV[9],
  'upstream_index_key', KEYS[2],
  'key_fingerprint', ARGV[10],
  'model_slug', ARGV[11],
  'protocol', ARGV[12]
)
if failure_status == '' then
  redis.call('HDEL', KEYS[1], 'failure_status')
else
  redis.call('HSET', KEYS[1], 'failure_status', failure_status)
end
redis.call(
  'HDEL', KEYS[1],
  'half_open_lease', 'half_open_generation', 'half_open_expires_at_ms',
  'half_open_exclusive_until_ms', 'reconcile_pending'
)
if cooldown_ms > 0 then
  redis.call('HSET', KEYS[1], 'cooldown_until_ms', tostring(now_ms + cooldown_ms))
else
  redis.call('HDEL', KEYS[1], 'cooldown_until_ms')
end
redis.call('EXPIRE', KEYS[1], ttl_seconds)
redis.call('ZADD', KEYS[2], now_ms, KEYS[1])
redis.call('ZADD', KEYS[3], now_ms, KEYS[1])
redis.call('EXPIRE', KEYS[2], ttl_seconds)
redis.call('EXPIRE', KEYS[3], ttl_seconds)
return 1
