local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local threshold_ms = tonumber(ARGV[1])
local members = redis.call('ZRANGE', KEYS[1], 0, -1)
local repaired = 0

for _, state_key in ipairs(members) do
  if redis.call('EXISTS', state_key) == 0 then
    redis.call('ZREM', KEYS[1], state_key)
  else
    local class = redis.call('HGET', state_key, 'failure_class')
    local status = redis.call('HGET', state_key, 'failure_status')
    local cooldown_until = tonumber(redis.call('HGET', state_key, 'cooldown_until_ms') or '0')
    if class == 'concurrency_saturated'
        and (not status or status == '')
        and cooldown_until - now_ms > threshold_ms then
      local upstream_index = redis.call('HGET', state_key, 'upstream_index_key')
      redis.call('DEL', state_key)
      redis.call('ZREM', KEYS[1], state_key)
      if upstream_index and upstream_index ~= '' then
        redis.call('ZREM', upstream_index, state_key)
      end
      repaired = repaired + 1
    end
  end
end

return {#members, repaired}
