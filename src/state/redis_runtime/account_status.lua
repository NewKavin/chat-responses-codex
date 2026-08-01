local operation = ARGV[1]
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)

if operation == 'acquire_poller' then
  local owner_token = ARGV[2]
  local ttl_ms = tonumber(ARGV[3])
  local current = redis.call('GET', KEYS[1])
  if not current then
    local acquired = redis.call('SET', KEYS[1], owner_token, 'PX', ttl_ms, 'NX')
    if acquired then return {0, 1} end
    return {0, 0}
  end
  if current == owner_token then
    redis.call('PEXPIRE', KEYS[1], ttl_ms)
    return {0, 1}
  end
  return {0, 0}
end

if operation == 'store' then
  local concurrency = tonumber(ARGV[2])
  local concurrency_limit = tonumber(ARGV[3])
  local freshness_ms = tonumber(ARGV[4])
  if not concurrency or not concurrency_limit or
      concurrency < 0 or concurrency_limit <= 0 or
      concurrency > concurrency_limit or
      math.floor(concurrency) ~= concurrency or
      math.floor(concurrency_limit) ~= concurrency_limit then
    return {3}
  end
  local fresh_until = now_ms + freshness_ms
  redis.call('HSET', KEYS[2],
    'concurrency', concurrency,
    'concurrency_limit', concurrency_limit,
    'observed_at', now_ms,
    'fresh_until', fresh_until)
  redis.call('PEXPIRE', KEYS[2], freshness_ms + 60000)

  if concurrency < concurrency_limit then
    redis.call('HDEL', KEYS[3], 'cooldown_until')
  end
  return {0, concurrency, concurrency_limit, now_ms, fresh_until}
end

if operation == 'read' then
  local fresh_until = tonumber(redis.call('HGET', KEYS[2], 'fresh_until') or '0')
  if fresh_until <= now_ms then
    redis.call('DEL', KEYS[2])
    return {1}
  end
  return {0,
    tonumber(redis.call('HGET', KEYS[2], 'concurrency')),
    tonumber(redis.call('HGET', KEYS[2], 'concurrency_limit')),
    tonumber(redis.call('HGET', KEYS[2], 'observed_at')),
    fresh_until}
end

return {3}
