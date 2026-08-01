local removed = redis.call('ZREM', KEYS[1], ARGV[1])
if KEYS[2] then
  redis.call('ZREM', KEYS[2], ARGV[1])
end
return removed
