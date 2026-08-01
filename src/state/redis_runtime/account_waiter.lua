local operation = ARGV[1]
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)

local function decode_ticket(value)
  if not value then
    return nil
  end
  local ok, ticket = pcall(cjson.decode, value)
  if not ok then
    return nil
  end
  return ticket
end

local function prune()
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

prune()

if operation == 'register' then
  local request_id = ARGV[2]
  local downstream_id = ARGV[3]
  local downstream_lease_id = ARGV[4]
  local waiter_budget_ms = tonumber(ARGV[5])
  local waiter_ttl_ms = tonumber(ARGV[6])
  local registration_token = ARGV[7]
  local existing = decode_ticket(redis.call('HGET', KEYS[2], request_id))
  if existing and existing.registration_token == registration_token then
    return {0, tonumber(existing.generation), tonumber(existing.registered_at_ms)}
  end
  redis.call('ZREM', KEYS[1], request_id)
  redis.call('HDEL', KEYS[2], request_id)
  local sequence = redis.call('INCR', KEYS[3])
  local generation = tonumber(redis.call('HGET', KEYS[4], 'generation') or '0')
  local ticket = {
    request_id = request_id,
    downstream_id = downstream_id,
    downstream_lease_id = downstream_lease_id,
    generation = generation,
    registration_token = registration_token,
    registered_at_ms = now_ms,
    logical_deadline = now_ms + waiter_budget_ms,
    lease_deadline = now_ms + waiter_ttl_ms
  }
  redis.call('HSET', KEYS[2], request_id, cjson.encode(ticket))
  redis.call('ZADD', KEYS[1], sequence, request_id)
  local retention_ms = waiter_ttl_ms + 600000
  redis.call('PEXPIRE', KEYS[1], retention_ms)
  redis.call('PEXPIRE', KEYS[2], retention_ms)
  redis.call('PEXPIRE', KEYS[3], retention_ms)
  if redis.call('PTTL', KEYS[4]) < retention_ms then
    redis.call('PEXPIRE', KEYS[4], retention_ms)
  end
  return {0, generation, now_ms}
end

if operation == 'renew' then
  local request_id = ARGV[2]
  local generation = tonumber(ARGV[3])
  local registered_at_ms = tonumber(ARGV[4])
  local waiter_ttl_ms = tonumber(ARGV[5])
  local raw = redis.call('HGET', KEYS[2], request_id)
  local ticket = decode_ticket(raw)
  if not ticket then
    return {3}
  end
  if tonumber(ticket.generation) ~= generation or tonumber(ticket.registered_at_ms) ~= registered_at_ms then
    return {2}
  end
  if tonumber(ticket.logical_deadline) <= now_ms or tonumber(ticket.lease_deadline) <= now_ms then
    redis.call('ZREM', KEYS[1], request_id)
    redis.call('HDEL', KEYS[2], request_id)
    return {3}
  end
  ticket.lease_deadline = now_ms + waiter_ttl_ms
  redis.call('HSET', KEYS[2], request_id, cjson.encode(ticket))
  return {0}
end

if operation == 'cancel' then
  local request_id = ARGV[2]
  local generation = tonumber(ARGV[3])
  local registered_at_ms = tonumber(ARGV[4])
  local ticket = decode_ticket(redis.call('HGET', KEYS[2], request_id))
  if not ticket then
    return {0}
  end
  if tonumber(ticket.generation) ~= generation or tonumber(ticket.registered_at_ms) ~= registered_at_ms then
    return {2}
  end
  redis.call('ZREM', KEYS[1], request_id)
  redis.call('HDEL', KEYS[2], request_id)
  return {0}
end

if operation == 'head' then
  local head = redis.call('ZRANGE', KEYS[1], 0, 0)
  if #head == 0 then
    return {0, ''}
  end
  return {0, head[1]}
end

if operation == 'count' then
  return {0, redis.call('ZCARD', KEYS[1])}
end

return {3}
