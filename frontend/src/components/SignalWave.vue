<template>
  <canvas ref="canvasRef" class="signal-wave" aria-hidden="true"></canvas>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'

const props = withDefaults(
  defineProps<{
    /** number of layered waves */
    layers?: number
    /** traveling packet dots */
    packets?: number
    /** overall opacity multiplier 0..1 */
    intensity?: number
  }>(),
  { layers: 3, packets: 10, intensity: 1 }
)

const canvasRef = ref<HTMLCanvasElement | null>(null)
let rafId = 0
let resizeObserver: ResizeObserver | null = null
let themeObserver: MutationObserver | null = null

const prefersReducedMotion = () =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches

const start = (canvas: HTMLCanvasElement) => {
  const ctx = canvas.getContext('2d')
  if (!ctx) return

  let width = 0
  let height = 0
  let accent = '47, 224, 168'

  const readAccent = () => {
    const raw = getComputedStyle(document.documentElement)
      .getPropertyValue('--crc-accent')
      .trim()
    const match = raw.match(/#([0-9a-f]{6})/i)
    if (match) {
      const value = parseInt(match[1], 16)
      accent = `${(value >> 16) & 255}, ${(value >> 8) & 255}, ${value & 255}`
    }
  }
  readAccent()

  const layers = Math.max(1, props.layers)
  const configs = Array.from({ length: layers }, (_, index) => ({
    amplitude: 0.09 - index * 0.02,
    frequency: 1.4 + index * 0.7,
    speed: (index % 2 === 0 ? 1 : -1) * (0.0004 - index * 0.00008),
    alpha: (0.42 - index * 0.12) * props.intensity,
    y: 0.5 + index * 0.12
  }))

  const dots = Array.from({ length: Math.max(0, props.packets) }, (_, index) => ({
    layer: index % layers,
    t: Math.random(),
    speed: 0.0012 + Math.random() * 0.0022
  }))

  const resize = () => {
    const rect = canvas.getBoundingClientRect()
    const dpr = Math.min(window.devicePixelRatio || 1, 2)
    width = Math.max(1, Math.floor(rect.width))
    height = Math.max(1, Math.floor(rect.height))
    canvas.width = width * dpr
    canvas.height = height * dpr
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
  }
  resize()

  const waveY = (cfg: (typeof configs)[number], x: number, time: number) => {
    const phase = time * cfg.speed * Math.PI * 2
    return (
      height * cfg.y +
      Math.sin((x / width) * Math.PI * 2 * cfg.frequency + phase) *
        height *
        cfg.amplitude *
        Math.sin((x / width) * Math.PI)
    )
  }

  const drawFrame = (time: number) => {
    ctx.clearRect(0, 0, width, height)
    configs.forEach((cfg, index) => {
      ctx.beginPath()
      for (let x = 0; x <= width; x += 3) {
        const y = waveY(cfg, x, time + index * 3600)
        if (x === 0) ctx.moveTo(x, y)
        else ctx.lineTo(x, y)
      }
      ctx.strokeStyle = `rgba(${accent}, ${Math.max(0.04, cfg.alpha)})`
      ctx.lineWidth = index === 0 ? 1.5 : 1
      ctx.shadowColor = `rgba(${accent}, 0.5)`
      ctx.shadowBlur = index === 0 ? 10 : 0
      ctx.stroke()
      ctx.shadowBlur = 0
    })
    for (const dot of dots) {
      dot.t += dot.speed / 10
      if (dot.t > 1) dot.t -= 1
      const cfg = configs[dot.layer]
      const x = dot.t * width
      const y = waveY(cfg, x, time + dot.layer * 3600)
      ctx.beginPath()
      ctx.arc(x, y, dot.layer === 0 ? 2 : 1.3, 0, Math.PI * 2)
      ctx.fillStyle = `rgba(${accent}, ${dot.layer === 0 ? 0.85 : 0.4})`
      ctx.shadowColor = `rgba(${accent}, 0.9)`
      ctx.shadowBlur = 7
      ctx.fill()
      ctx.shadowBlur = 0
    }
  }

  if (prefersReducedMotion()) {
    drawFrame(1400)
    return
  }

  const loop = (time: number) => {
    drawFrame(time)
    rafId = requestAnimationFrame(loop)
  }
  rafId = requestAnimationFrame(loop)

  resizeObserver = new ResizeObserver(resize)
  resizeObserver.observe(canvas)

  themeObserver = new MutationObserver(readAccent)
  themeObserver.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['class']
  })
}

onMounted(() => {
  if (canvasRef.value) start(canvasRef.value)
})

onBeforeUnmount(() => {
  cancelAnimationFrame(rafId)
  resizeObserver?.disconnect()
  themeObserver?.disconnect()
})
</script>

<style scoped>
.signal-wave {
  display: block;
  width: 100%;
  height: 100%;
  pointer-events: none;
}
</style>
