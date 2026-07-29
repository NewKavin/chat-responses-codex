local time = redis.call('TIME')
local now_seconds = tonumber(time[1])
local now_ms = (now_seconds * 1000) + math.floor(time[2] / 1000)
local request_window_seconds = tonumber(ARGV[1])
local retention_seconds = math.max(60, request_window_seconds)

redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
local expired_events = redis.call(
  'ZRANGEBYSCORE', KEYS[2], '-inf', now_ms - (retention_seconds * 1000)
)
if #expired_events > 0 then
  redis.call('HDEL', KEYS[3], unpack(expired_events))
end
redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now_ms - (retention_seconds * 1000))

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
  tostring(now_seconds)
}
