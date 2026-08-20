local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local lease_id = ARGV[1]
local ttl_seconds = tonumber(ARGV[2])
local lease_duration_ms = tonumber(ARGV[3])
local probe_interval_ms = tonumber(ARGV[4])

local function state_value(key, field)
  return redis.call('HGET', key, field)
end

-- The key must not be cooling and must not carry an active lease, exactly
-- like the regular reserve path: a credentials/quota quarantine is not a
-- probeable route outage.
local key_class = state_value(KEYS[1], 'failure_class')
if key_class then
  local key_cooldown_until = tonumber(state_value(KEYS[1], 'cooldown_until_ms') or '0')
  if key_cooldown_until > now_ms then
    return {'1', key_class, tostring(math.max(1, key_cooldown_until - now_ms)), state_value(KEYS[1], 'failure_status') or ''}
  end
  local key_active_lease = state_value(KEYS[1], 'half_open_lease')
  if key_active_lease and key_active_lease ~= '' then
    local key_expires_at = tonumber(state_value(KEYS[1], 'half_open_expires_at_ms') or '0')
    if key_expires_at > now_ms then
      return {'2', key_class, '1000', state_value(KEYS[1], 'failure_status') or ''}
    end
    redis.call(
      'HDEL', KEYS[1],
      'half_open_lease', 'half_open_generation', 'half_open_expires_at_ms',
      'half_open_exclusive_until_ms'
    )
  end
end

-- The route must exist and still be cooling: the last-resort probe only
-- applies when the normal reserve path refuses the route.  Unlike the
-- regular reserve, the remaining cooldown itself is ignored.
local route_class = state_value(KEYS[2], 'failure_class')
if not route_class then
  return {'1', 'transient_server', '0', ''}
end
local route_cooldown_until = tonumber(state_value(KEYS[2], 'cooldown_until_ms') or '0')
if route_cooldown_until <= now_ms then
  return {'1', route_class, '0', state_value(KEYS[2], 'failure_status') or ''}
end
local route_active_lease = state_value(KEYS[2], 'half_open_lease')
if route_active_lease and route_active_lease ~= '' then
  local route_expires_at = tonumber(state_value(KEYS[2], 'half_open_expires_at_ms') or '0')
  if route_expires_at > now_ms then
    return {'2', route_class, '1000', state_value(KEYS[2], 'failure_status') or ''}
  end
  redis.call(
    'HDEL', KEYS[2],
    'half_open_lease', 'half_open_generation', 'half_open_expires_at_ms',
    'half_open_exclusive_until_ms'
  )
end

-- Per-route minimum interval between early probes (aligned with the 1s
-- optimistic half-open poll interval).
local last_early_probe_ms = tonumber(state_value(KEYS[2], 'last_early_probe_ms') or '0')
if last_early_probe_ms > 0 and now_ms - last_early_probe_ms < probe_interval_ms then
  local remaining = last_early_probe_ms + probe_interval_ms - now_ms
  return {'2', route_class, tostring(math.max(1, remaining)), state_value(KEYS[2], 'failure_status') or ''}
end

-- Grant the single-flight early half-open lease on the route.
local route_state_generation = state_value(KEYS[2], 'state_generation') or ''
local route_generation = state_value(KEYS[2], 'state_generation') or '0'
-- Early probes stay strictly single-flight: the exclusive window for a
-- probe-held lease is the full lease lifetime, so regular reserves cannot
-- bypass a cooling route's probe.
redis.call('HSET', KEYS[2],
  'half_open_lease', lease_id,
  'half_open_generation', route_generation,
  'half_open_expires_at_ms', tostring(now_ms + lease_duration_ms),
  'half_open_exclusive_until_ms', tostring(now_ms + lease_duration_ms),
  'last_early_probe_ms', tostring(now_ms),
  'last_access_ms', tostring(now_ms)
)
redis.call('EXPIRE', KEYS[2], ttl_seconds)
redis.call('ZADD', KEYS[4], now_ms, KEYS[2])
redis.call('ZADD', KEYS[6], now_ms, KEYS[2])
redis.call('EXPIRE', KEYS[4], ttl_seconds)
redis.call('EXPIRE', KEYS[6], ttl_seconds)

-- Grant the key lease only when the key has a failure class whose cooldown
-- already elapsed (mirrors the regular reserve script).
local key_generation = ''
if key_class then
  key_generation = state_value(KEYS[1], 'state_generation') or '0'
  redis.call('HSET', KEYS[1],
    'half_open_lease', lease_id,
    'half_open_generation', key_generation,
    'half_open_expires_at_ms', tostring(now_ms + lease_duration_ms),
    'half_open_exclusive_until_ms', tostring(now_ms + lease_duration_ms),
    'last_access_ms', tostring(now_ms)
  )
  redis.call('EXPIRE', KEYS[1], ttl_seconds)
  redis.call('ZADD', KEYS[3], now_ms, KEYS[1])
  redis.call('ZADD', KEYS[5], now_ms, KEYS[1])
  redis.call('EXPIRE', KEYS[3], ttl_seconds)
  redis.call('EXPIRE', KEYS[5], ttl_seconds)
end

return {'0', key_generation, route_generation, route_state_generation, '1'}
