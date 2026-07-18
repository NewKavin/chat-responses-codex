<template>
  <span class="count-up-value">{{ display }}</span>
</template>

<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue'

const props = withDefaults(
  defineProps<{
    value: number | string
    duration?: number
  }>(),
  { duration: 700 }
)

const parseNumber = (value: number | string): number | null => {
  if (typeof value === 'number') return Number.isFinite(value) ? value : null
  const normalized = value.replace(/,/g, '').trim()
  if (normalized === '') return null
  const parsed = Number(normalized)
  return Number.isFinite(parsed) ? parsed : null
}

const format = (value: number) => Math.round(value).toLocaleString()

const display = ref(String(props.value))
let rafId = 0

const prefersReducedMotion = () =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches

const animateTo = (from: number, to: number) => {
  if (typeof window === 'undefined' || prefersReducedMotion() || from === to) {
    display.value = format(to)
    return
  }
  cancelAnimationFrame(rafId)
  const start = performance.now()
  const tick = (now: number) => {
    const progress = Math.min(1, (now - start) / props.duration)
    const eased = 1 - Math.pow(1 - progress, 3)
    display.value = format(from + (to - from) * eased)
    if (progress < 1) rafId = requestAnimationFrame(tick)
  }
  rafId = requestAnimationFrame(tick)
}

watch(
  () => props.value,
  (next, prev) => {
    const to = parseNumber(next)
    if (to === null) {
      cancelAnimationFrame(rafId)
      display.value = String(next)
      return
    }
    const from = parseNumber(prev ?? 0) ?? 0
    animateTo(from, to)
  },
  { immediate: true }
)

onBeforeUnmount(() => cancelAnimationFrame(rafId))
</script>

<style scoped>
.count-up-value {
  font-variant-numeric: tabular-nums;
}
</style>
