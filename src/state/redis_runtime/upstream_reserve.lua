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

if hedge then
  if redis.call('ZCARD', KEYS[1]) >= max_concurrency then
    local oldest = redis.call('ZRANGE', KEYS[1], 0, 0, 'WITHSCORES')
    local retry_after = 1
    if #oldest >= 2 then
      retry_after = math.max(1, math.ceil((tonumber(oldest[2]) - now_ms) / 1000))
    end
    return {'1', tostring(retry_after)}
  end

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
redis.call('ZADD', KEYS[2], now_ms, event_id)
redis.call('HSET', KEYS[3], event_id, ARGV[3])
redis.call('EXPIRE', KEYS[2], retention_seconds + 60)
redis.call('EXPIRE', KEYS[3], retention_seconds + 60)
return {'0'}
