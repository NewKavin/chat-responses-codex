-- Atomic downstream admission: request-quota reservation + concurrency lease.
-- Keys:
--   KEYS[1] = requests zset (event_id -> score in ms)
--   KEYS[2] = tokens zset (event_id -> score in ms)
--   KEYS[3] = token_values hash (event_id -> token count)
--   KEYS[4] = group leases zset (lease_id -> expiry ms)
--   KEYS[5] = downstream-wide aggregate leases zset (global backstop, C7)
-- Args:
--   ARGV[1] = event_id
--   ARGV[2] = per_minute_limit
--   ARGV[3] = request_window_seconds (0 = disabled)
--   ARGV[4] = request_quota (0 = disabled)
--   ARGV[5] = daily_limit (0 = disabled)
--   ARGV[6] = monthly_limit (0 = disabled)
--   ARGV[7] = lease_id
--   ARGV[8] = global concurrency limit (downstream.max_concurrency)
--   ARGV[9] = lease_duration_ms
--   ARGV[10] = group_limit (0 = no group matched / no group cap)
-- Returns:
--   {0}                              success (request + lease reserved)
--   {1, used, limit, retry}          per-minute limit
--   {2, used, limit, retry, window}  request quota
--   {3, used, limit, retry}          daily token/cost limit
--   {4, used, limit, retry}          monthly token/cost limit
--   {5, retry, limit}                global concurrency backstop (nothing recorded)
--   {6, retry, limit}                group concurrency cap (nothing recorded)
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local event_id = ARGV[1]
local minute_limit = tonumber(ARGV[2])
local request_window_seconds = tonumber(ARGV[3])
local request_quota = tonumber(ARGV[4])
local daily_limit = tonumber(ARGV[5])
local monthly_limit = tonumber(ARGV[6])
local lease_id = ARGV[7]
local concurrency_limit = tonumber(ARGV[8])
local lease_duration_ms = tonumber(ARGV[9])
local group_limit = tonumber(ARGV[10])

local request_retention_seconds = math.max(60, request_window_seconds)
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms - (request_retention_seconds * 1000))

local token_retention_seconds = 60
if monthly_limit > 0 then
  token_retention_seconds = 30 * 24 * 60 * 60
elseif daily_limit > 0 then
  token_retention_seconds = 24 * 60 * 60
end
local expired_tokens = redis.call(
  'ZRANGEBYSCORE', KEYS[2], '-inf', now_ms - (token_retention_seconds * 1000)
)
if #expired_tokens > 0 then
  redis.call('HDEL', KEYS[3], unpack(expired_tokens))
end
redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now_ms - (token_retention_seconds * 1000))

-- Idempotent replay: the script is atomic, so a re-run after a connection
-- timeout sees both the request event and the lease already present.
if redis.call('ZSCORE', KEYS[1], event_id) then
  return {0}
end

local function retry_after(key, start_ms, window_seconds)
  local oldest = redis.call('ZRANGEBYSCORE', key, start_ms, '+inf', 'WITHSCORES', 'LIMIT', 0, 1)
  if #oldest < 2 then
    return 1
  end
  return math.max(1, math.ceil(((tonumber(oldest[2]) + (window_seconds * 1000)) - now_ms) / 1000))
end

local minute_start = now_ms - (60 * 1000) + 1
local minute_used = redis.call('ZCOUNT', KEYS[1], minute_start, '+inf')
if minute_used >= minute_limit then
  return {1, minute_used, minute_limit, retry_after(KEYS[1], minute_start, 60)}
end

if request_window_seconds > 0 and request_quota > 0 then
  local request_start = now_ms - (request_window_seconds * 1000) + 1
  local request_used = redis.call('ZCOUNT', KEYS[1], request_start, '+inf')
  if request_used >= request_quota then
    return {
      2,
      request_used,
      request_quota,
      retry_after(KEYS[1], request_start, request_window_seconds),
      request_window_seconds
    }
  end
end

local function token_usage(start_ms)
  local ids = redis.call('ZRANGEBYSCORE', KEYS[2], start_ms, '+inf', 'WITHSCORES')
  local total = 0
  for index = 1, #ids, 2 do
    local id = ids[index]
    total = total + tonumber(redis.call('HGET', KEYS[3], id) or '0')
  end
  return total, ids
end

local function token_retry_after(ids, window_seconds, required_tokens)
  local released = 0
  for index = 1, #ids, 2 do
    local id = ids[index]
    released = released + tonumber(redis.call('HGET', KEYS[3], id) or '0')
    if released >= required_tokens then
      local expires_at_ms = tonumber(ids[index + 1]) + (window_seconds * 1000)
      return math.max(1, math.ceil((expires_at_ms - now_ms) / 1000))
    end
  end
  return 1
end

if daily_limit > 0 then
  local daily_start = now_ms - (24 * 60 * 60 * 1000) + 1
  local daily_used, daily_ids = token_usage(daily_start)
  if daily_used >= daily_limit then
    local required_tokens = daily_used + 1 - daily_limit
    return {
      3,
      daily_used,
      daily_limit,
      token_retry_after(daily_ids, 24 * 60 * 60, required_tokens)
    }
  end
end

if monthly_limit > 0 then
  local monthly_seconds = 30 * 24 * 60 * 60
  local monthly_start = now_ms - (monthly_seconds * 1000) + 1
  local monthly_used, monthly_ids = token_usage(monthly_start)
  if monthly_used >= monthly_limit then
    local required_tokens = monthly_used + 1 - monthly_limit
    return {
      4,
      monthly_used,
      monthly_limit,
      token_retry_after(monthly_ids, monthly_seconds, required_tokens)
    }
  end
end

-- Concurrency lease: prune expired leases from both zsets, then admit only
-- when the group cap (C7) and the downstream-wide global backstop are both
-- satisfied. Order mirrors the local backend: group first, then global.
redis.call('ZREMRANGEBYSCORE', KEYS[4], '-inf', now_ms)
redis.call('ZREMRANGEBYSCORE', KEYS[5], '-inf', now_ms)
if redis.call('ZSCORE', KEYS[4], lease_id) then
  return {0}
end
if group_limit > 0 and redis.call('ZCARD', KEYS[4]) >= group_limit then
  local oldest = redis.call('ZRANGE', KEYS[4], 0, 0, 'WITHSCORES')
  local retry_after = 1
  if #oldest >= 2 then
    retry_after = math.max(1, math.ceil((tonumber(oldest[2]) - now_ms) / 1000))
  end
  return {6, retry_after, group_limit}
end
if redis.call('ZCARD', KEYS[5]) >= concurrency_limit then
  local oldest = redis.call('ZRANGE', KEYS[5], 0, 0, 'WITHSCORES')
  local retry_after = 1
  if #oldest >= 2 then
    retry_after = math.max(1, math.ceil((tonumber(oldest[2]) - now_ms) / 1000))
  end
  return {5, retry_after, concurrency_limit}
end

redis.call('ZADD', KEYS[1], now_ms, event_id)
redis.call('EXPIRE', KEYS[1], request_retention_seconds + 60)
redis.call('ZADD', KEYS[4], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[4], lease_duration_ms + 60000)
redis.call('ZADD', KEYS[5], now_ms + lease_duration_ms, lease_id)
redis.call('PEXPIRE', KEYS[5], lease_duration_ms + 60000)
return {0}
