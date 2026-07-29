local time = redis.call('TIME')
local now_seconds = tonumber(time[1])
local action = ARGV[1]

if action == 'clear' then
  redis.call('DEL', KEYS[1])
  return 0
end

local cooldown_seconds = math.max(1, tonumber(ARGV[2]))
local cooldown_until = now_seconds + cooldown_seconds
local current_until = tonumber(redis.call('HGET', KEYS[1], 'cooldown_until') or '0')
cooldown_until = math.max(current_until, cooldown_until)
redis.call('HSET', KEYS[1],
  'cooldown_until', tostring(cooldown_until),
  'last_feedback_type', ARGV[3],
  'last_retry_after_seconds', tostring(cooldown_seconds)
)
redis.call('EXPIRE', KEYS[1], math.max(60, cooldown_until - now_seconds + 60))
return cooldown_until
