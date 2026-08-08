local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local threshold_ms = tonumber(ARGV[1])
local capacity = tonumber(ARGV[2])
local members = redis.call('ZRANGE', KEYS[1], 0, math.max(0, capacity - 1))
local counts = {}

for _, state_key in ipairs(members) do
  if redis.call('EXISTS', state_key) == 1 then
    local class = redis.call('HGET', state_key, 'failure_class')
    local status = redis.call('HGET', state_key, 'failure_status')
    local cooldown_until = tonumber(redis.call('HGET', state_key, 'cooldown_until_ms') or '0')
    if class == 'concurrency_saturated'
        and (not status or status == '')
        and cooldown_until - now_ms > threshold_ms then
      local upstream_id = redis.call('HGET', state_key, 'upstream_id')
      if upstream_id and upstream_id ~= '' then
        counts[upstream_id] = (counts[upstream_id] or 0) + 1
      end
    end
  end
end

local upstream_ids = {}
for upstream_id, _ in pairs(counts) do
  table.insert(upstream_ids, upstream_id)
end
table.sort(upstream_ids)

local result = {}
for _, upstream_id in ipairs(upstream_ids) do
  table.insert(result, upstream_id)
  table.insert(result, tostring(counts[upstream_id]))
end
return result
