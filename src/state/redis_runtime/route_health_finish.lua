local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local lease_id = ARGV[1]
local key_generation = ARGV[2]
local route_generation = ARGV[3]
local route_state_generation = ARGV[4]
local outcome = ARGV[5]
local class = ARGV[6]
local explicit_retry_ms = tonumber(ARGV[7])
local streak_reset_ms = tonumber(ARGV[8])
local ttl_seconds = tonumber(ARGV[9])
local global_capacity = tonumber(ARGV[10])
local upstream_capacity = tonumber(ARGV[11])
local failure_status = ARGV[16]

local committed_result = redis.call('GET', KEYS[8])
if committed_result then
  if committed_result ~= '0' and committed_result ~= '1' then
    return redis.error_reply('invalid route health finish marker')
  end
  return tonumber(committed_result)
end

local function commit_result(result)
  redis.call('SET', KEYS[8], tostring(result), 'EX', ttl_seconds)
  return result
end

local cursor = 17
local route_schedule_count = tonumber(ARGV[cursor])
cursor = cursor + 1
local route_schedule = {}
for index = 1, route_schedule_count do
  route_schedule[index] = tonumber(ARGV[cursor])
  cursor = cursor + 1
end
local key_schedule_count = tonumber(ARGV[cursor])
cursor = cursor + 1
local key_schedule = {}
for index = 1, key_schedule_count do
  key_schedule[index] = tonumber(ARGV[cursor])
  cursor = cursor + 1
end
local probe_schedule_count = tonumber(ARGV[cursor])
cursor = cursor + 1
local probe_schedule = {}
for index = 1, probe_schedule_count do
  probe_schedule[index] = tonumber(ARGV[cursor])
  cursor = cursor + 1
end

local function owns(key, generation)
  if generation == '' then
    return true
  end
  return redis.call('HGET', key, 'half_open_lease') == lease_id
    and (redis.call('HGET', key, 'half_open_generation') or '') == generation
    and (redis.call('HGET', key, 'state_generation') or '') == generation
end

if not owns(KEYS[1], key_generation) or not owns(KEYS[2], route_generation) then
  return commit_result(0)
end

local function clear_state(key, upstream_index, global_index)
  redis.call('DEL', key)
  redis.call('ZREM', upstream_index, key)
  redis.call('ZREM', global_index, key)
end

local function release_half_open(key, generation, upstream_index, global_index)
  if generation == '' then
    return
  end
  redis.call(
    'HDEL', key,
    'half_open_lease', 'half_open_generation', 'half_open_expires_at_ms'
  )
  redis.call('HSET', key, 'last_access_ms', tostring(now_ms))
  redis.call('EXPIRE', key, ttl_seconds)
  redis.call('ZADD', upstream_index, now_ms, key)
  redis.call('ZADD', global_index, now_ms, key)
  redis.call('EXPIRE', upstream_index, ttl_seconds)
  redis.call('EXPIRE', global_index, ttl_seconds)
end

local function prune_index(index_key)
  local members = redis.call('ZRANGE', index_key, 0, -1)
  for _, member in ipairs(members) do
    if redis.call('EXISTS', member) == 0 then
      redis.call('ZREM', index_key, member)
    end
  end
end

local function evict_one(index_key, other_index)
  local members = redis.call('ZRANGE', index_key, 0, -1)
  for _, member in ipairs(members) do
    local active = redis.call('HGET', member, 'half_open_lease')
    if not active or active == '' then
      local upstream_index = redis.call('HGET', member, 'upstream_index_key')
      redis.call('DEL', member)
      redis.call('ZREM', index_key, member)
      redis.call('ZREM', other_index, member)
      if upstream_index and upstream_index ~= '' then
        redis.call('ZREM', upstream_index, member)
      end
      return true
    end
  end
  return false
end

local function ensure_capacity(state_key, upstream_index, global_index)
  if redis.call('EXISTS', state_key) == 1 then
    return true
  end
  prune_index(upstream_index)
  prune_index(global_index)
  if redis.call('ZCARD', upstream_index) >= upstream_capacity
      and not evict_one(upstream_index, global_index) then
    return false
  end
  if redis.call('ZCARD', global_index) >= global_capacity
      and not evict_one(global_index, upstream_index) then
    return false
  end
  return true
end

local function schedule_value(schedule, count, step)
  if count == 0 then
    return 0
  end
  return schedule[math.min(step, count)]
end

local function observe(
  state_key,
  upstream_index,
  global_index,
  failure_class,
  schedule,
  schedule_count,
  exact_retry,
  model_slug,
  protocol,
  half_open_probe
)
  if not ensure_capacity(state_key, upstream_index, global_index) then
    return false
  end
  local previous_class = redis.call('HGET', state_key, 'failure_class')
  local previous_at = tonumber(redis.call('HGET', state_key, 'last_failure_ms') or '0')
  local previous_count = tonumber(redis.call('HGET', state_key, 'failure_count') or '0')
  local step = 1
  if previous_class == failure_class and now_ms - previous_at <= streak_reset_ms then
    step = math.max(1, previous_count + 1)
    if half_open_probe and failure_class ~= 'concurrency_saturated' then
      -- A half-open probe failure must not escalate the streak without
      -- bound (B3): cap the step so the cooldown cannot pin at the
      -- 5-minute maximum while the route keeps failing probes.
      -- ConcurrencySaturated is exempt: it follows its own bounded probe
      -- schedule and never escalates to the max.
      step = math.min(step, 5)
    end
  end
  local cooldown_ms = schedule_value(schedule, schedule_count, step)
  if explicit_retry_ms >= 0 then
    if exact_retry then
      cooldown_ms = explicit_retry_ms
    else
      cooldown_ms = math.max(cooldown_ms, explicit_retry_ms)
    end
  end
  local generation = redis.call('INCR', KEYS[7])
  redis.call('HSET', state_key,
    'failure_count', tostring(step),
    'failure_class', failure_class,
    'last_failure_ms', tostring(now_ms),
    'cooldown_until_ms', tostring(now_ms + cooldown_ms),
    'state_generation', tostring(generation),
    'last_access_ms', tostring(now_ms),
    'upstream_id', ARGV[12],
    'upstream_index_key', upstream_index,
    'key_fingerprint', ARGV[13],
    'model_slug', model_slug,
    'protocol', protocol
  )
  if failure_status == '' then
    redis.call('HDEL', state_key, 'failure_status')
  else
    redis.call('HSET', state_key, 'failure_status', failure_status)
  end
  redis.call(
    'HDEL', state_key,
    'half_open_lease', 'half_open_generation', 'half_open_expires_at_ms',
    'reconcile_pending'
  )
  redis.call('EXPIRE', state_key, ttl_seconds)
  redis.call('ZADD', upstream_index, now_ms, state_key)
  redis.call('ZADD', global_index, now_ms, state_key)
  redis.call('EXPIRE', upstream_index, ttl_seconds)
  redis.call('EXPIRE', global_index, ttl_seconds)
  return true
end

local function reapply_concurrency_probe()
  if route_generation == ''
      or redis.call('HGET', KEYS[2], 'failure_class') ~= 'concurrency_saturated' then
    return false
  end
  local step = tonumber(redis.call('HGET', KEYS[2], 'failure_count') or '1')
  local delay_ms = schedule_value(probe_schedule, probe_schedule_count, step)
  redis.call(
    'HDEL', KEYS[2],
    'half_open_lease', 'half_open_generation', 'half_open_expires_at_ms'
  )
  redis.call('HSET', KEYS[2],
    'cooldown_until_ms', tostring(now_ms + delay_ms),
    'last_access_ms', tostring(now_ms)
  )
  redis.call('EXPIRE', KEYS[2], ttl_seconds)
  redis.call('ZADD', KEYS[4], now_ms, KEYS[2])
  redis.call('ZADD', KEYS[6], now_ms, KEYS[2])
  redis.call('EXPIRE', KEYS[4], ttl_seconds)
  redis.call('EXPIRE', KEYS[6], ttl_seconds)
  return true
end

local key_reconcile_pending = redis.call('HGET', KEYS[1], 'reconcile_pending') == '1'
local route_reconcile_pending = redis.call('HGET', KEYS[2], 'reconcile_pending') == '1'
if key_reconcile_pending then
  clear_state(KEYS[1], KEYS[3], KEYS[5])
end
if route_reconcile_pending then
  clear_state(KEYS[2], KEYS[4], KEYS[6])
end

if outcome == 'success' then
  if not route_reconcile_pending and route_generation ~= '' then
    clear_state(KEYS[2], KEYS[4], KEYS[6])
  elseif not route_reconcile_pending and route_state_generation ~= ''
      and (redis.call('HGET', KEYS[2], 'state_generation') or '') == route_state_generation then
    clear_state(KEYS[2], KEYS[4], KEYS[6])
  end
  if not key_reconcile_pending and key_generation ~= '' then
    clear_state(KEYS[1], KEYS[3], KEYS[5])
  end
elseif outcome == 'route_failure' or outcome == 'route_failure_with_retry' then
  if not key_reconcile_pending then
    release_half_open(KEYS[1], key_generation, KEYS[3], KEYS[5])
  end
  if not route_reconcile_pending then
    if route_schedule_count == 0 then
      clear_state(KEYS[2], KEYS[4], KEYS[6])
    elseif not observe(
      KEYS[2], KEYS[4], KEYS[6], class,
      route_schedule, route_schedule_count,
      class == 'concurrency_saturated' and explicit_retry_ms >= 0,
      ARGV[14], ARGV[15], route_generation ~= ''
    ) then
      return -1
    end
  end
elseif outcome == 'key_failure' or outcome == 'key_failure_with_retry' then
  if not route_reconcile_pending then
    release_half_open(KEYS[2], route_generation, KEYS[4], KEYS[6])
  end
  if not key_reconcile_pending then
    if key_schedule_count == 0 then
      clear_state(KEYS[1], KEYS[3], KEYS[5])
    elseif not observe(
      KEYS[1], KEYS[3], KEYS[5], class,
      key_schedule, key_schedule_count, false, '', '', key_generation ~= ''
    ) then
      return -1
    end
  end
elseif outcome == 'uncertain_route_failure' then
  if not key_reconcile_pending then
    release_half_open(KEYS[1], key_generation, KEYS[3], KEYS[5])
  end
  if not route_reconcile_pending and not reapply_concurrency_probe() then
    if route_schedule_count == 0 then
      clear_state(KEYS[2], KEYS[4], KEYS[6])
    elseif not observe(
        KEYS[2], KEYS[4], KEYS[6], class,
        route_schedule, route_schedule_count, false,
        ARGV[14], ARGV[15], route_generation ~= ''
    ) then
      return -1
    end
  end
elseif outcome == 'cancelled' then
  if not route_reconcile_pending and not reapply_concurrency_probe() then
    release_half_open(KEYS[2], route_generation, KEYS[4], KEYS[6])
  end
  if not key_reconcile_pending then
    release_half_open(KEYS[1], key_generation, KEYS[3], KEYS[5])
  end
end
return commit_result(1)
