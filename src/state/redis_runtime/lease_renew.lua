-- Extend a lease's expiry. Primary zset must hold the lease (else no-op / 0).
-- When KEYS[2] (downstream global aggregate, C7) also holds it, extend that
-- member too so the cross-group global backstop stays trustworthy for long
-- requests; upstream callers pass only KEYS[1] and skip the aggregate.
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local lease_id = ARGV[1]
local lease_duration_ms = tonumber(ARGV[2])
local renewed = 0
if redis.call('ZSCORE', KEYS[1], lease_id) then
  redis.call('ZADD', KEYS[1], now_ms + lease_duration_ms, lease_id)
  redis.call('PEXPIRE', KEYS[1], lease_duration_ms + 60000)
  renewed = 1
end
if KEYS[2] then
  if redis.call('ZSCORE', KEYS[2], lease_id) then
    redis.call('ZADD', KEYS[2], now_ms + lease_duration_ms, lease_id)
    redis.call('PEXPIRE', KEYS[2], lease_duration_ms + 60000)
  end
end
return renewed
