-- Atomic downstream concurrency lease reservation (C7).
-- Keys:
--   KEYS[1] = group lease zset (lease_id -> expiry ms); the legacy no-group
--             bucket (`...:leases`) when the model matched no group.
--   KEYS[2] = downstream-wide aggregate lease zset (`...:leases_all`) used
--             for the global backstop across every group bucket.
-- Args:
--   ARGV[1] = lease_id
--   ARGV[2] = group_limit (0 = no group matched / no group cap)
--   ARGV[3] = global_limit (downstream.max_concurrency)
--   ARGV[4] = lease_duration_ms
-- Returns:
--   {0}               success (lease reserved in both zsets)
--   {1, retry_after}  group cap exceeded
--   {2, retry_after}  global backstop exceeded
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local lease_id = ARGV[1]
local group_limit = tonumber(ARGV[2])
local global_limit = tonumber(ARGV[3])
local lease_duration_ms = tonumber(ARGV[4])
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now_ms)
if redis.call('ZSCORE', KEYS[1], lease_id) or redis.call('ZSCORE', KEYS[2], lease_id) then
  return {0}
end
local function retry_after_for(key)
  local oldest = redis.call('ZRANGE', key, 0, 0, 'WITHSCORES')
  if #oldest < 2 then
    return 1
  end
  return math.max(1, math.ceil((tonumber(oldest[2]) - now_ms) / 1000))
end
if group_limit > 0 and redis.call('ZCARD', KEYS[1]) >= group_limit then
  return {1, retry_after_for(KEYS[1])}
end
if redis.call('ZCARD', KEYS[2]) >= global_limit then
  return {2, retry_after_for(KEYS[2])}
end
redis.call('ZADD', KEYS[1], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[1], lease_duration_ms + 60000)
redis.call('ZADD', KEYS[2], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[2], lease_duration_ms + 60000)
return {0}
