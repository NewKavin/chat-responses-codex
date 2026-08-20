local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local lease_id = ARGV[1]
local ttl_seconds = tonumber(ARGV[2])
local lease_duration_ms = tonumber(ARGV[3])
local legacy_threshold_ms = tonumber(ARGV[4])
-- Half-open exclusivity window (T1): while `half_open_exclusive_until_ms`
-- is in the future, concurrent callers are rejected as busy; after it
-- elapses they are admitted without a lease even though the original lease
-- is still alive. 0 disables the window entirely.
local exclusive_window_ms = tonumber(ARGV[5])

local function state_value(key, field)
  return redis.call('HGET', key, field)
end

local function clear_lease(key)
  redis.call(
    'HDEL', key,
    'half_open_lease', 'half_open_generation', 'half_open_expires_at_ms',
    'half_open_exclusive_until_ms'
  )
end

local function blocked(key)
  local class = state_value(key, 'failure_class')
  if not class then
    return nil
  end
  local cooldown_until = tonumber(state_value(key, 'cooldown_until_ms') or '0')
  if cooldown_until > now_ms then
    return {'1', class, tostring(math.max(1, cooldown_until - now_ms)), state_value(key, 'failure_status') or ''}
  end
  local active_lease = state_value(key, 'half_open_lease')
  if active_lease and active_lease ~= '' then
    local expires_at = tonumber(state_value(key, 'half_open_expires_at_ms') or '0')
    if expires_at > now_ms then
      -- Lease alive: busy only while the exclusive window is open; an
      -- elapsed window admits concurrent callers without a lease.
      local exclusive_until = tonumber(state_value(key, 'half_open_exclusive_until_ms') or '0')
      if exclusive_until > now_ms then
        return {'2', class, '1000', state_value(key, 'failure_status') or ''}
      end
    else
      -- Lease expired: it may be taken over (and is cleaned up here).
      clear_lease(key)
    end
  end
  return nil
end

-- A grant is only possible when the state does not carry a live lease:
-- after the exclusive window elapses, callers are admitted without a lease
-- and must not overwrite the original probe's lease (`half_open==false`),
-- mirroring the local registry.
local function can_grant(key)
  local active_lease = state_value(key, 'half_open_lease')
  if active_lease and active_lease ~= '' then
    local expires_at = tonumber(state_value(key, 'half_open_expires_at_ms') or '0')
    return expires_at <= now_ms
  end
  return true
end

local function refresh_indexes(state_key, upstream_index, global_index)
  redis.call('ZADD', upstream_index, now_ms, state_key)
  redis.call('ZADD', global_index, now_ms, state_key)
  redis.call('EXPIRE', upstream_index, ttl_seconds)
  redis.call('EXPIRE', global_index, ttl_seconds)
end

local function clear_legacy_local_admission(key, upstream_index, global_index)
  local class = redis.call('HGET', key, 'failure_class')
  local status = redis.call('HGET', key, 'failure_status')
  local cooldown_until = tonumber(redis.call('HGET', key, 'cooldown_until_ms') or '0')
  if class == 'concurrency_saturated'
      and (not status or status == '')
      and cooldown_until - now_ms > legacy_threshold_ms then
    redis.call('DEL', key)
    redis.call('ZREM', upstream_index, key)
    redis.call('ZREM', global_index, key)
    return true
  end
  return false
end

clear_legacy_local_admission(KEYS[2], KEYS[4], KEYS[6])

local key_blocked = blocked(KEYS[1])
if key_blocked then
  return key_blocked
end
local route_blocked = blocked(KEYS[2])
if route_blocked then
  return route_blocked
end

local route_state_generation = state_value(KEYS[2], 'state_generation') or ''
local key_generation = ''
local route_generation = ''

if can_grant(KEYS[1]) and state_value(KEYS[1], 'failure_class') then
  key_generation = state_value(KEYS[1], 'state_generation') or '0'
  redis.call('HSET', KEYS[1],
    'half_open_lease', lease_id,
    'half_open_generation', key_generation,
    'half_open_expires_at_ms', tostring(now_ms + lease_duration_ms),
    'half_open_exclusive_until_ms', tostring(now_ms + exclusive_window_ms),
    'last_access_ms', tostring(now_ms)
  )
  redis.call('EXPIRE', KEYS[1], ttl_seconds)
  refresh_indexes(KEYS[1], KEYS[3], KEYS[5])
end

if can_grant(KEYS[2]) and state_value(KEYS[2], 'failure_class') then
  route_generation = state_value(KEYS[2], 'state_generation') or '0'
  redis.call('HSET', KEYS[2],
    'half_open_lease', lease_id,
    'half_open_generation', route_generation,
    'half_open_expires_at_ms', tostring(now_ms + lease_duration_ms),
    'half_open_exclusive_until_ms', tostring(now_ms + exclusive_window_ms),
    'last_access_ms', tostring(now_ms)
  )
  redis.call('EXPIRE', KEYS[2], ttl_seconds)
  refresh_indexes(KEYS[2], KEYS[4], KEYS[6])
end

local half_open = '0'
if key_generation ~= '' or route_generation ~= '' then
  half_open = '1'
end
return {'0', key_generation, route_generation, route_state_generation, half_open}
