-- E4.4: read-only per-account lease census.
--
-- The Redis backend enforces the upstream concurrency cap inside
-- `upstream_reserve.lua` against the per-account lease index (KEYS[1]), but
-- the gateway's C3 slot queue and the C4.2 `gateway_concurrency_saturated`
-- details used to read the *in-process* lease table, which the Redis path
-- never writes.  That made the queue's "is a slot free yet?" poll always see
-- 0 (so it returned immediately without waiting) and pinned every diagnostic
-- field to 0.  This script gives both call sites the real numbers.
--
-- Purely observational: it must never add, remove or re-score a lease.  The
-- lazy sweeps stay in `upstream_reserve.lua`, so a caller polling this script
-- cannot perturb admission accounting.
--
-- KEYS[1] - per-account lease index (sorted set, score = expiry in ms)
-- ARGV[1] - lease duration in ms (score = last heartbeat + duration)
-- ARGV[2] - stale-after threshold in ms
--
-- Returns {live, stale} as strings:
--   live  - leases that have not passed their expiry score yet.  Expired
--           leases are excluded here rather than swept, so this matches what
--           the reserve script will count once it prunes.
--   stale - live leases whose last heartbeat is older than the stale window,
--           i.e. the ones the reserve script's stale sweep would reclaim on
--           its next pass.  Same range arithmetic as that sweep: score =
--           last_heartbeat + lease_duration, so "heartbeat older than
--           stale_after" is score < now + lease_duration - stale_after.
local time = redis.call('TIME')
local now_ms = (time[1] * 1000) + math.floor(time[2] / 1000)
local lease_duration_ms = tonumber(ARGV[1])
local stale_after_ms = tonumber(ARGV[2])

local live = redis.call('ZCOUNT', KEYS[1], '(' .. now_ms, '+inf')

local stale = 0
if stale_after_ms < lease_duration_ms then
  local stale_cutoff = now_ms + lease_duration_ms - stale_after_ms
  stale = redis.call('ZCOUNT', KEYS[1], '(' .. now_ms, stale_cutoff)
end

return {tostring(live), tostring(stale)}
