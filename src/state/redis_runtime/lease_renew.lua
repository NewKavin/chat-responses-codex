local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local lease_id = ARGV[1]
local lease_duration_ms = tonumber(ARGV[2])
if not redis.call('ZSCORE', KEYS[1], lease_id) then
  return 0
end
redis.call('ZADD', KEYS[1], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[1], lease_duration_ms + 60000)
return 1
