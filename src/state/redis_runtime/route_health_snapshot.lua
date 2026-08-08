local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local action = ARGV[1]

local function state_values(key)
  if redis.call('EXISTS', key) == 0 then
    return nil
  end
  local cooldown_until = tonumber(redis.call('HGET', key, 'cooldown_until_ms') or '0')
  local active = redis.call('HGET', key, 'half_open_lease')
  local half_open_expires_at = tonumber(redis.call('HGET', key, 'half_open_expires_at_ms') or '0')
  local half_open_remaining = 0
  if active and active ~= '' and half_open_expires_at > now_ms then
    half_open_remaining = half_open_expires_at - now_ms
  end
  return {
    key,
    redis.call('HGET', key, 'failure_count') or '0',
    redis.call('HGET', key, 'failure_class') or '',
    tostring(math.max(0, cooldown_until - now_ms)),
    active and active ~= '' and '1' or '0',
    tostring(half_open_remaining),
    redis.call('HGET', key, 'upstream_id') or '',
    redis.call('HGET', key, 'key_fingerprint') or '',
    redis.call('HGET', key, 'model_slug') or '',
    redis.call('HGET', key, 'protocol') or ''
  }
end

if action == 'one' then
  local values = state_values(KEYS[1])
  if not values then
    return {'0'}
  end
  table.remove(values, 1)
  table.insert(values, 1, '1')
  return values
end

local result = {}
local members
if action == 'many' then
  members = KEYS
elseif action == 'all' then
  members = redis.call('ZRANGE', KEYS[1], 0, -1)
else
  return redis.error_reply('invalid route health snapshot action')
end
for _, member in ipairs(members) do
  local values = state_values(member)
  if values then
    for _, value in ipairs(values) do
      table.insert(result, value)
    end
  elseif action == 'all' then
    redis.call('ZREM', KEYS[1], member)
  end
end
return result
