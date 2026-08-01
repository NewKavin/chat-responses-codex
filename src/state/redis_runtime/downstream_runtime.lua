local operation = ARGV[1]
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)

local function prune()
  redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
  redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now_ms)
  local waiting = redis.call('ZRANGE', KEYS[2], 0, -1)
  for _, lease_id in ipairs(waiting) do
    if not redis.call('ZSCORE', KEYS[1], lease_id) then
      redis.call('ZREM', KEYS[2], lease_id)
    end
  end
end

prune()

if operation == 'mark_waiting' then
  local lease_id = ARGV[2]
  local expires_at_ms = tonumber(ARGV[3])
  local admitted_until = redis.call('ZSCORE', KEYS[1], lease_id)
  if not admitted_until then
    return 0
  end
  local deadline = math.min(tonumber(admitted_until), expires_at_ms)
  redis.call('ZADD', KEYS[2], deadline, lease_id)
  redis.call('PEXPIREAT', KEYS[2], deadline + 60000)
  return 1
end

if operation == 'unmark_waiting' then
  return redis.call('ZREM', KEYS[2], ARGV[2])
end

if operation == 'release' then
  local lease_id = ARGV[2]
  local admitted = redis.call('ZREM', KEYS[1], lease_id)
  redis.call('ZREM', KEYS[2], lease_id)
  return admitted
end

if operation == 'snapshot' then
  return {redis.call('ZCARD', KEYS[1]), redis.call('ZCARD', KEYS[2])}
end

return redis.error_reply('unsupported downstream runtime operation')
