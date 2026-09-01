-- Release one upstream account lease and record its hold duration (E5.3).
--
-- KEYS[1] = per-account lease zset      KEYS[2] = per-upstream aggregate zset
-- KEYS[3] = reserve-instant index       KEYS[4] = bounded hold-sample reservoir
-- ARGV[1] = lease id  ARGV[2] = sample cap  ARGV[3] = reservoir TTL (seconds)
--
-- Split from `lease_release.lua` on purpose: that script is shared with the
-- downstream group path, whose KEYS[3] is the global aggregate.  Overloading
-- the same positions for the hold-sample keys would have made one script mean
-- two different things depending on the caller.
local removed = redis.call('ZREM', KEYS[1], ARGV[1])
redis.call('ZREM', KEYS[2], ARGV[1])

local reserved_at = redis.call('ZSCORE', KEYS[3], ARGV[1])
-- Drop the reserve instant unconditionally so the index cannot accumulate
-- entries for leases that are never released through this path.
redis.call('ZREM', KEYS[3], ARGV[1])

-- Sample only a release that actually removed a live lease: a double release,
-- or one whose lease was already reclaimed as leaked/stale, would otherwise
-- contribute a hold that no request ever experienced.
if removed == 1 and reserved_at then
  local time = redis.call('TIME')
  local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
  local hold_ms = now_ms - tonumber(reserved_at)
  if hold_ms >= 0 then
    -- Score is the hold duration, so percentiles are a plain ZRANGE.  The
    -- member must be unique per sample or two equal holds would collapse into
    -- one entry and skew the distribution; the lease id supplies that.
    redis.call('ZADD', KEYS[4], hold_ms, ARGV[1])
    local cap = tonumber(ARGV[2])
    local size = redis.call('ZCARD', KEYS[4])
    if cap > 0 and size > cap then
      -- A score-ordered set has no insertion order to evict by, so trim the
      -- low end: the p95 lives in the tail, and dropping the shortest holds
      -- keeps that tail intact.
      redis.call('ZREMRANGEBYRANK', KEYS[4], 0, size - cap - 1)
    end
    redis.call('EXPIRE', KEYS[4], tonumber(ARGV[3]))
  end
end
return removed
