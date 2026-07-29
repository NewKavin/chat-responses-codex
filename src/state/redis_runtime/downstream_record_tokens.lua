local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local retention_seconds = math.max(60, tonumber(ARGV[3]))
redis.call('ZADD', KEYS[1], now_ms, ARGV[1])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[2])
redis.call('EXPIRE', KEYS[1], retention_seconds + 60)
redis.call('EXPIRE', KEYS[2], retention_seconds + 60)
return 1
