local time = redis.call('TIME')
local now_seconds = tonumber(time[1])
local now_ms = (now_seconds * 1000) + math.floor(time[2] / 1000)
local request_window_seconds = tonumber(ARGV[1])
local retention_seconds = math.max(60, request_window_seconds)
-- F1.4: current upstream lease TTL and stale-after window (both in ms).  The
-- ZSET score is the lease expiry time, so `score - lease_duration_ms` is the
-- last heartbeat time for a live lease.
local lease_duration_ms = tonumber(ARGV[2])
local stale_after_ms = tonumber(ARGV[3])
local function count_new_reclaims(lease_ids)
  if #lease_ids == 0 then
    return 0
  end
  local new_reclaims = redis.call('SADD', KEYS[6], unpack(lease_ids))
  -- Keep the deduplication set only for the period in which the two lease
  -- indexes can disagree after a lazy sweep.  It is not a lease registry.
  redis.call('PEXPIRE', KEYS[6], math.max(lease_duration_ms + 60000, retention_seconds * 1000 + 60000))
  return new_reclaims
end

-- F1.4: an expired aggregate lease can be removed by a snapshot even when no
-- new admission follows. Count that lazy reclaim so the diagnostic counter is
-- not dependent on another request arriving after the leak.
local expired_leases = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now_ms)
if #expired_leases > 0 then
  redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
  local new_reclaims = count_new_reclaims(expired_leases)
  if new_reclaims > 0 then
    redis.call('HINCRBY', KEYS[5], 'leaked_reclaimed', new_reclaims)
  end
end
local expired_events = redis.call(
  'ZRANGEBYSCORE', KEYS[2], '-inf', now_ms - (retention_seconds * 1000)
)
if #expired_events > 0 then
  redis.call('HDEL', KEYS[3], unpack(expired_events))
end
redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now_ms - (retention_seconds * 1000))

-- F1.4: stale leases are live (not yet expired) members whose last heartbeat
-- is older than `stale_after_ms`, i.e. score < now + ttl - stale_after.  The
-- counts/age below are computed from the pre-reclamation member set (like the
-- local backend's pre-sweep reporting), then the stale members are reclaimed
-- right here and counted on the counters hash (F1.5).
local stale_lease_count = 0
if stale_after_ms < lease_duration_ms then
  stale_lease_count = redis.call(
    'ZCOUNT', KEYS[1], '(' .. now_ms, now_ms + lease_duration_ms - stale_after_ms
  )
end
local oldest_lease_age_seconds = 0
local oldest = redis.call('ZRANGE', KEYS[1], 0, 0, 'WITHSCORES')
if #oldest >= 2 then
  local oldest_score = tonumber(oldest[2])
  oldest_lease_age_seconds = math.max(
    0,
    math.floor((now_ms + lease_duration_ms - oldest_score) / 1000)
  )
end
if stale_lease_count > 0 then
  local stale_leases = redis.call(
    'ZRANGEBYSCORE', KEYS[1], '(' .. now_ms, now_ms + lease_duration_ms - stale_after_ms
  )
  redis.call(
    'ZREMRANGEBYSCORE', KEYS[1], '(' .. now_ms, now_ms + lease_duration_ms - stale_after_ms
  )
  local new_reclaims = count_new_reclaims(stale_leases)
  if new_reclaims > 0 then
    redis.call('HINCRBY', KEYS[5], 'stale_reclaimed', new_reclaims)
  end
end

local function cost_since(start_ms)
  local ids = redis.call('ZRANGEBYSCORE', KEYS[2], start_ms, '+inf')
  local total = 0
  for _, id in ipairs(ids) do
    total = total + tonumber(redis.call('HGET', KEYS[3], id) or '0')
  end
  return total
end

local cooldown_until = tonumber(redis.call('HGET', KEYS[4], 'cooldown_until') or '0')
if cooldown_until <= now_seconds then
  cooldown_until = 0
end

return {
  tostring(redis.call('ZCARD', KEYS[1])),
  string.format('%.17g', cost_since(now_ms - (60 * 1000) + 1)),
  string.format('%.17g', cost_since(now_ms - (request_window_seconds * 1000) + 1)),
  tostring(cooldown_until),
  redis.call('HGET', KEYS[4], 'last_feedback_type') or '',
  redis.call('HGET', KEYS[4], 'last_retry_after_seconds') or '',
  tostring(now_seconds),
  redis.call('HGET', KEYS[5], 'leaked_reclaimed') or '0',
  redis.call('HGET', KEYS[5], 'stale_reclaimed') or '0',
  redis.call('HGET', KEYS[5], 'capacity_reject') or '0',
  tostring(stale_lease_count),
  tostring(oldest_lease_age_seconds),
  redis.call('HGET', KEYS[5], 'sse_bad_frame_skipped') or '0',
  redis.call('HGET', KEYS[5], 'sse_parse_error') or '0',
  redis.call('HGET', KEYS[5], 'transport_decode_error') or '0',
  redis.call('HGET', KEYS[5], 'route_cooldown_skipped') or '0'
}
