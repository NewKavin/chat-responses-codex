-- Atomically drop a lease from the primary zset and (optionally) an
-- aggregate / waiting companion zset. Used by both the upstream path
-- (KEYS[1]=account lease, KEYS[2]=aggregate) and the downstream path
-- (KEYS[1]=group lease, KEYS[2]=waiting, KEYS[3]=global aggregate, C7).
-- Upstream callers omit KEYS[3] (nil -> skipped).
local removed = redis.call('ZREM', KEYS[1], ARGV[1])
if KEYS[2] then
  redis.call('ZREM', KEYS[2], ARGV[1])
end
if KEYS[3] then
  redis.call('ZREM', KEYS[3], ARGV[1])
end
return removed
