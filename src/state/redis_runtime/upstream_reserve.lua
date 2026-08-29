local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local event_id = ARGV[1]
local lease_id = ARGV[2]
local request_cost = tonumber(ARGV[3])
local hedge = ARGV[4] == '1'
local max_concurrency = tonumber(ARGV[5])
local minute_limit = tonumber(ARGV[6])
local request_window_seconds = tonumber(ARGV[7])
local request_quota = tonumber(ARGV[8])
local lease_duration_ms = tonumber(ARGV[9])
local stale_after_ms = tonumber(ARGV[10])
local retention_seconds = math.max(60, request_window_seconds)

-- F1.4: expired leases are lazily pruned here; the removal is counted on the
-- per-upstream counters hash so the snapshot can report `leaked_reclaimed_total`
-- instead of a hard-coded 0.
local expired_leases = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now_ms)
if #expired_leases > 0 then
  redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
  redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now_ms)
  redis.call('HINCRBY', KEYS[5], 'leaked_reclaimed', #expired_leases)
end
-- F1.5: reclaim leases whose last heartbeat is older than the stale window
-- *before* their TTL expiry (score = expiry, so last heartbeat = score − TTL;
-- stale means now < score < now + TTL − stale_after).  The expired sweep above
-- already removed score <= now, so this range only touches live-but-stale
-- leases.  Counted separately from leaked (TTL-expired) reclamations.
local stale_lease_count = 0
if stale_after_ms < lease_duration_ms then
  local stale_cutoff = now_ms + lease_duration_ms - stale_after_ms
  stale_lease_count = #redis.call('ZRANGEBYSCORE', KEYS[1], '(' .. now_ms, stale_cutoff)
  if stale_lease_count > 0 then
    redis.call('ZREMRANGEBYSCORE', KEYS[1], '(' .. now_ms, stale_cutoff)
    redis.call('ZREMRANGEBYSCORE', KEYS[2], '(' .. now_ms, stale_cutoff)
    redis.call('HINCRBY', KEYS[5], 'stale_reclaimed', stale_lease_count)
  end
end
local expired_events = redis.call(
  'ZRANGEBYSCORE', KEYS[3], '-inf', now_ms - (retention_seconds * 1000)
)
if #expired_events > 0 then
  redis.call('HDEL', KEYS[4], unpack(expired_events))
end
redis.call('ZREMRANGEBYSCORE', KEYS[3], '-inf', now_ms - (retention_seconds * 1000))

local existing_account_lease = redis.call('ZSCORE', KEYS[1], lease_id)
local existing_aggregate_lease = redis.call('ZSCORE', KEYS[2], lease_id)
local existing_event = redis.call('ZSCORE', KEYS[3], event_id)
local existing_cost = redis.call('HGET', KEYS[4], event_id)
if existing_account_lease and existing_aggregate_lease and existing_event and existing_cost then
  if existing_cost == ARGV[3] then
    return {'0'}
  end
  return {'4', '1'}
end
if existing_account_lease or existing_aggregate_lease or existing_event or existing_cost then
  return {'4', '1'}
end

local function cost_since(start_ms)
  local ids = redis.call('ZRANGEBYSCORE', KEYS[3], start_ms, '+inf')
  local total = 0
  for _, id in ipairs(ids) do
    total = total + tonumber(redis.call('HGET', KEYS[4], id) or '0')
  end
  return total
end

-- The concurrency cap applies to every request (main + hedge), so a
-- saturated upstream rejects new work up front instead of relying solely
-- on provider 429s and the reactive account-probe throttle.
if redis.call('ZCARD', KEYS[1]) >= max_concurrency then
  -- F1.4: the Redis admission gate is the backend's counterpart of the local
  -- pre-dispatch gate; count its rejections so `capacity_reject_total` is real.
  redis.call('HINCRBY', KEYS[5], 'capacity_reject', 1)
  return {'1', '1'}
end

if hedge then
  local minute_cost = cost_since(now_ms - (60 * 1000) + 1)
  if minute_limit > 0 and minute_cost + request_cost > minute_limit then
    return {'2', '1'}
  end

  local window_cost = cost_since(now_ms - (request_window_seconds * 1000) + 1)
  if request_quota > 0 and window_cost + request_cost > request_quota then
    return {'3', '1'}
  end
end

redis.call('ZADD', KEYS[1], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[1], lease_duration_ms + 60000)
redis.call('ZADD', KEYS[2], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[2], lease_duration_ms + 60000)
redis.call('ZADD', KEYS[3], now_ms, event_id)
redis.call('HSET', KEYS[4], event_id, ARGV[3])
redis.call('EXPIRE', KEYS[3], retention_seconds + 60)
redis.call('EXPIRE', KEYS[4], retention_seconds + 60)
return {'0'}
