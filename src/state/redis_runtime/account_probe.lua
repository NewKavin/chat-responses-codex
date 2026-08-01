local operation = ARGV[1]
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)

local function decode_ticket(value)
  if not value then return nil end
  local ok, ticket = pcall(cjson.decode, value)
  if not ok then return nil end
  return ticket
end

local function prune_waiters()
  local entries = redis.call('HGETALL', KEYS[2])
  for index = 1, #entries, 2 do
    local request_id = entries[index]
    local ticket = decode_ticket(entries[index + 1])
    if not ticket or tonumber(ticket.logical_deadline) <= now_ms or tonumber(ticket.lease_deadline) <= now_ms then
      redis.call('ZREM', KEYS[1], request_id)
      redis.call('HDEL', KEYS[2], request_id)
    end
  end
end

local function prune_probe()
  local expires_at = tonumber(redis.call('HGET', KEYS[4], 'expires_at') or '0')
  if expires_at > 0 and expires_at <= now_ms then
    redis.call('DEL', KEYS[4])
    redis.call('HINCRBY', KEYS[3], 'generation', 1)
  end
end

local function local_delay(identity, generation, jitter_max, delay_count, delays_start)
  local rejection_count = tonumber(redis.call('HGET', KEYS[3], 'rejection_count') or '0')
  local index = math.min(rejection_count, delay_count - 1)
  local delay = tonumber(ARGV[delays_start + index])
  local jitter = 0
  if jitter_max > 0 then
    local digest = redis.sha1hex(identity .. ':' .. tostring(generation))
    jitter = tonumber(string.sub(digest, 1, 8), 16) % (jitter_max + 1)
  end
  return delay + jitter
end

local function apply_rejection(identity, retry_after_ms, jitter_max, delay_count, delays_start)
  local generation = redis.call('HINCRBY', KEYS[3], 'generation', 1)
  local delay = local_delay(identity, generation, jitter_max, delay_count, delays_start)
  redis.call('HINCRBY', KEYS[3], 'rejection_count', 1)
  local local_deadline = now_ms + delay
  local explicit_deadline = tonumber(redis.call('HGET', KEYS[3], 'explicit_until') or '0')
  if retry_after_ms >= 0 then
    explicit_deadline = math.max(explicit_deadline, now_ms + retry_after_ms)
    redis.call('HSET', KEYS[3], 'explicit_until', explicit_deadline)
  end
  local cooldown = math.max(local_deadline, explicit_deadline)
  redis.call('HSET', KEYS[3], 'cooldown_until', cooldown, 'saturated', 1, 'last_access', now_ms)
  return generation
end

local function retain_state()
  local cooldown = tonumber(redis.call('HGET', KEYS[3], 'cooldown_until') or '0')
  local explicit = tonumber(redis.call('HGET', KEYS[3], 'explicit_until') or '0')
  local deadline = math.max(cooldown, explicit)
  local retention_ms = math.max(1200000, math.max(0, deadline - now_ms) + 600000)
  local current_ttl = redis.call('PTTL', KEYS[3])
  if current_ttl < retention_ms then redis.call('PEXPIRE', KEYS[3], retention_ms) end
end

prune_waiters()
prune_probe()

if operation == 'reject' then
  local identity = ARGV[2]
  local retry_after_ms = tonumber(ARGV[3])
  local jitter_max = tonumber(ARGV[4])
  local delay_count = tonumber(ARGV[5])
  if delay_count <= 0 then return {3} end
  local replay_generation = redis.call('GET', KEYS[5])
  if replay_generation then
    return {0, tonumber(replay_generation)}
  end
  local generation = apply_rejection(identity, retry_after_ms, jitter_max, delay_count, 7)
  redis.call('SET', KEYS[5], generation, 'PX', 60000)
  retain_state()
  return {0, generation}
end

if operation == 'grant' then
  local request_id = ARGV[2]
  local ticket_generation = tonumber(ARGV[3])
  local registered_at_ms = tonumber(ARGV[4])
  local registration_token = ARGV[5]
  local owner_token = ARGV[6]
  local probe_ttl_ms = tonumber(ARGV[7])
  local existing_request = redis.call('HGET', KEYS[4], 'request_id')
  local existing_owner = redis.call('HGET', KEYS[4], 'owner_token')
  local existing_expires = tonumber(redis.call('HGET', KEYS[4], 'expires_at') or '0')
  if existing_request == request_id and existing_owner == owner_token and existing_expires > now_ms then
    return {0,
      tonumber(redis.call('HGET', KEYS[4], 'generation')),
      owner_token,
      existing_expires}
  end
  local ticket = decode_ticket(redis.call('HGET', KEYS[2], request_id))
  if not ticket then return {3} end
  if tonumber(ticket.generation) ~= ticket_generation or
      tonumber(ticket.registered_at_ms) ~= registered_at_ms or
      ticket.registration_token ~= registration_token then
    return {2}
  end
  local head = redis.call('ZRANGE', KEYS[1], 0, 0)
  if #head == 0 or head[1] ~= request_id then return {1, 100} end
  local probe_expires = tonumber(redis.call('HGET', KEYS[4], 'expires_at') or '0')
  if probe_expires > now_ms then return {1, probe_expires - now_ms} end
  local cooldown = tonumber(redis.call('HGET', KEYS[3], 'cooldown_until') or '0')
  local explicit = tonumber(redis.call('HGET', KEYS[3], 'explicit_until') or '0')
  local deadline = math.max(cooldown, explicit)
  if deadline > now_ms then return {1, deadline - now_ms} end

  redis.call('ZREM', KEYS[1], request_id)
  redis.call('HDEL', KEYS[2], request_id)
  local generation = tonumber(redis.call('HGET', KEYS[3], 'generation') or '0')
  local expires_at = now_ms + probe_ttl_ms
  redis.call('HSET', KEYS[4],
    'request_id', request_id,
    'generation', generation,
    'owner_token', owner_token,
    'expires_at', expires_at)
  redis.call('HSET', KEYS[3], 'last_access', now_ms)
  redis.call('PEXPIRE', KEYS[4], probe_ttl_ms + 60000)
  local state_ttl = probe_ttl_ms + 600000
  if redis.call('PTTL', KEYS[3]) < state_ttl then redis.call('PEXPIRE', KEYS[3], state_ttl) end
  return {0, generation, owner_token, expires_at}
end

if operation == 'renew' then
  local request_id = ARGV[2]
  local generation = tonumber(ARGV[3])
  local owner_token = ARGV[4]
  local probe_ttl_ms = tonumber(ARGV[5])
  if tonumber(redis.call('HGET', KEYS[3], 'generation') or '-1') ~= generation then return {2} end
  if redis.call('HGET', KEYS[4], 'request_id') ~= request_id then return {2} end
  if redis.call('HGET', KEYS[4], 'owner_token') ~= owner_token then return {2} end
  local expires_at = tonumber(redis.call('HGET', KEYS[4], 'expires_at') or '0')
  if expires_at <= now_ms then return {2} end
  expires_at = now_ms + probe_ttl_ms
  redis.call('HSET', KEYS[4], 'expires_at', expires_at)
  redis.call('HSET', KEYS[3], 'last_access', now_ms)
  redis.call('PEXPIRE', KEYS[4], probe_ttl_ms + 60000)
  return {0, generation, owner_token, expires_at}
end

if operation == 'finish' then
  local request_id = ARGV[2]
  local generation = tonumber(ARGV[3])
  local owner_token = ARGV[4]
  local outcome = ARGV[5]
  if redis.call('GET', KEYS[5]) then return {0} end
  if tonumber(redis.call('HGET', KEYS[3], 'generation') or '-1') ~= generation then return {2} end
  if redis.call('HGET', KEYS[4], 'request_id') ~= request_id then return {2} end
  if redis.call('HGET', KEYS[4], 'owner_token') ~= owner_token then return {2} end
  local expires_at = tonumber(redis.call('HGET', KEYS[4], 'expires_at') or '0')
  if expires_at <= now_ms then return {2} end
  redis.call('DEL', KEYS[4])
  redis.call('HSET', KEYS[3], 'last_access', now_ms)

  if outcome == 'accepted' then
    redis.call('HDEL', KEYS[3], 'cooldown_until', 'explicit_until')
    redis.call('HSET', KEYS[3], 'rejection_count', 0, 'saturated', 0)
    redis.call('SET', KEYS[5], 1, 'PX', 60000)
    retain_state()
    return {0}
  end
  if outcome == 'concurrency_rejected' then
    local identity = ARGV[7]
    local retry_after_ms = tonumber(ARGV[8])
    local jitter_max = tonumber(ARGV[9])
    local delay_count = tonumber(ARGV[10])
    if delay_count <= 0 then return {3} end
    apply_rejection(identity, retry_after_ms, jitter_max, delay_count, 11)
    redis.call('SET', KEYS[5], 1, 'PX', 60000)
    retain_state()
    return {0}
  end
  if outcome == 'attempt_failed' or outcome == 'cancelled' then
    redis.call('SET', KEYS[5], 1, 'PX', 60000)
    retain_state()
    return {0}
  end
  return {3}
end

return {3}
