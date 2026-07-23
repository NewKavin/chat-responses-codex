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
        <stop offset="0%" stop-color="#1fd3a2" />
        <stop offset="100%" stop-color="#0a6d59" />
      </linearGradient>
      <linearGradient :id="sheenId" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0%" stop-color="#ffffff" stop-opacity="0.24" />
        <stop offset="48%" stop-color="#ffffff" stop-opacity="0" />
      </linearGradient>
    </defs>
    <rect width="64" height="64" rx="15" :fill="`url(#${gradientId})`" />
    <rect width="64" height="64" rx="15" :fill="`url(#${sheenId})`" />
    <rect
      x="1"
      y="1"
      width="62"
      height="62"
      rx="14"
      fill="none"
      stroke="#ffffff"
      stroke-opacity="0.16"
      stroke-width="1.5"
    />
    <g
      fill="none"
      stroke="#ffffff"
      stroke-width="4.4"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      <path d="M17.5 24.5h20" />
      <path d="M32.5 18.5l6 6-6 6" />
      <path d="M46.5 39.5h-20" />
      <path d="M31.5 33.5l-6 6 6 6" />
    </g>
    <circle cx="13.5" cy="24.5" r="2.6" fill="#ffffff" />
    <circle cx="50.5" cy="39.5" r="2.6" fill="#ffffff" />
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
  border-radius: 24%;
  box-shadow: 0 2px 10px rgb(10 109 89 / 32%),
    inset 0 0 0 1px rgb(255 255 255 / 8%);
}
</style>
