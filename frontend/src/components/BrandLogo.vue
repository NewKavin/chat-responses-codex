<template>
  <svg
    class="brand-logo"
    :width="size"
    :height="size"
    viewBox="0 0 64 64"
    role="img"
    aria-label="Chat Responses Codex"
    xmlns="http://www.w3.org/2000/svg"
  >
    <defs>
      <linearGradient :id="gradientId" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0%" stop-color="#17b48f" />
        <stop offset="100%" stop-color="#0a6d59" />
      </linearGradient>
      <linearGradient :id="sheenId" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0%" stop-color="#ffffff" stop-opacity="0.22" />
        <stop offset="45%" stop-color="#ffffff" stop-opacity="0" />
      </linearGradient>
    </defs>
    <rect width="64" height="64" rx="14" :fill="`url(#${gradientId})`" />
    <rect width="64" height="64" rx="14" :fill="`url(#${sheenId})`" />
    <g
      fill="none"
      stroke="#ffffff"
      stroke-width="4.6"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      <path d="M19 25.5h18.5" />
      <path d="M33.5 19.5l6 6-6 6" />
      <path d="M45 38.5H26.5" />
      <path d="M30.5 32.5l-6 6 6 6" />
    </g>
  </svg>
</template>

<script setup lang="ts">
let instanceSeq = 0

const props = withDefaults(
  defineProps<{
    size?: number | string
  }>(),
  { size: 34 }
)

const size = props.size
const seq = ++instanceSeq
const gradientId = `crc-brand-gradient-${seq}`
const sheenId = `crc-brand-sheen-${seq}`
</script>

<style scoped>
.brand-logo {
  display: block;
  flex: 0 0 auto;
  border-radius: 22%;
  box-shadow: 0 2px 6px rgb(10 109 89 / 28%);
}
</style>
