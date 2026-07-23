<template>
  <div class="gauge-ring" :style="{ width: `${size}px`, height: `${size}px` }" role="img"
    :aria-label="`${Math.round(clamped)}%`">
    <svg :width="size" :height="size" :viewBox="`0 0 ${size} ${size}`">
      <circle
        class="gauge-ring__track"
        :cx="center"
        :cy="center"
        :r="radius"
        :stroke-width="stroke"
        fill="none"
      />
      <!-- tick marks -->
      <g class="gauge-ring__ticks" aria-hidden="true">
        <line
          v-for="tick in 24"
          :key="tick"
          :x1="center"
          :y1="stroke / 2 + 1"
          :x2="center"
          :y2="stroke / 2 + (tick % 6 === 1 ? 6 : 3.5)"
          :transform="`rotate(${(tick - 1) * 15} ${center} ${center})`"
        />
      </g>
      <circle
        class="gauge-ring__bar"
        :class="toneClass"
        :cx="center"
        :cy="center"
        :r="radius"
        :stroke-width="stroke"
        fill="none"
        stroke-linecap="round"
        :stroke-dasharray="circumference"
        :stroke-dashoffset="dashOffset"
        :transform="`rotate(-90 ${center} ${center})`"
      />
    </svg>
    <div class="gauge-ring__center">
      <slot :value="Math.round(clamped)">
        <strong class="gauge-ring__value">{{ Math.round(clamped) }}<i>%</i></strong>
      </slot>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    value: number
    size?: number
    stroke?: number
  }>(),
  { size: 92, stroke: 5 }
)

const clamped = computed(() => Math.min(100, Math.max(0, props.value || 0)))
const center = computed(() => props.size / 2)
const radius = computed(() => (props.size - props.stroke) / 2 - 4)
const circumference = computed(() => 2 * Math.PI * radius.value)
const dashOffset = computed(
  () => circumference.value * (1 - clamped.value / 100)
)
const toneClass = computed(() => {
  if (clamped.value >= 90) return 'gauge-ring__bar--danger'
  if (clamped.value >= 70) return 'gauge-ring__bar--warning'
  return 'gauge-ring__bar--ok'
})
</script>

<style scoped>
.gauge-ring {
  position: relative;
  display: inline-grid;
  flex: 0 0 auto;
  place-items: center;
}

.gauge-ring svg {
  display: block;
}

.gauge-ring__track {
  stroke: var(--crc-border);
}

.gauge-ring__ticks line {
  stroke: var(--crc-border-strong);
  stroke-width: 1;
  opacity: 0.65;
}

.gauge-ring__bar {
  transition: stroke-dashoffset 900ms var(--crc-ease-expo),
    stroke 300ms var(--crc-ease);
}

.gauge-ring__bar--ok {
  stroke: var(--crc-accent);
  filter: drop-shadow(0 0 5px var(--crc-accent));
}

.gauge-ring__bar--warning {
  stroke: var(--crc-warning);
  filter: drop-shadow(0 0 5px var(--crc-warning));
}

.gauge-ring__bar--danger {
  stroke: var(--crc-danger);
  filter: drop-shadow(0 0 5px var(--crc-danger));
}

.gauge-ring__center {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
}

.gauge-ring__value {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 22px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.02em;
}

.gauge-ring__value i {
  margin-left: 2px;
  color: var(--crc-text-subtle);
  font-size: 11px;
  font-style: normal;
  font-weight: 500;
}
</style>
