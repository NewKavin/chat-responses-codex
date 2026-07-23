<template>
  <main class="auth-shell">
    <section class="auth-shell__stage" aria-hidden="true">
      <canvas ref="stageCanvas" class="auth-shell__canvas"></canvas>

      <div class="auth-shell__stage-inner">
        <div class="auth-shell__brand">
          <BrandLogo :size="38" class="auth-shell__brand-mark" />
          <div class="auth-shell__brand-copy">
            <strong>Chat Responses Codex</strong>
            <span>Gateway Console</span>
          </div>
        </div>

        <div class="auth-shell__headline">
          <p class="auth-shell__eyebrow">API GATEWAY // SIGNAL DECK</p>
          <h2 class="auth-shell__title">
            <span>每一次请求</span>
            <span class="auth-shell__title-accent">皆有回响</span>
          </h2>
          <p class="auth-shell__sub">
            REQUEST <MoveRight :size="12" :stroke-width="2" /> RESPONSE <MoveRight :size="12" :stroke-width="2" /> CODEX
          </p>
        </div>

        <div class="auth-shell__telemetry">
          <div class="auth-shell__telemetry-row">
            <span>PROTOCOL</span>
            <em>RESPONSES / CHAT.COMPLETIONS</em>
          </div>
          <div class="auth-shell__telemetry-row">
            <span>ROUTING</span>
            <em>MULTI-UPSTREAM // MULTI-DOWNSTREAM</em>
          </div>
          <div class="auth-shell__telemetry-row">
            <span>STATUS</span>
            <em class="auth-shell__telemetry-live">
              <b class="auth-shell__live-dot"></b>SIGNAL LOCKED
            </em>
          </div>
        </div>
      </div>
    </section>

    <section class="auth-shell__side">
      <div class="auth-shell__utility">
        <ThemeSwitcher />
      </div>

      <section class="auth-shell__panel" aria-labelledby="auth-shell-title">
        <header class="auth-shell__header">
          <p class="auth-shell__panel-eyebrow">AUTH // {{ title }}</p>
          <h1 id="auth-shell-title">{{ title }}</h1>
          <p>{{ subtitle }}</p>
        </header>

        <div class="auth-shell__content">
          <slot />
        </div>

        <footer v-if="$slots.footer" class="auth-shell__footer">
          <slot name="footer" />
        </footer>
      </section>
    </section>
  </main>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { MoveRight } from '@lucide/vue'
import ThemeSwitcher from '@/components/ThemeSwitcher.vue'
import BrandLogo from '@/components/BrandLogo.vue'

defineProps<{
  title: string
  subtitle: string
}>()

/* ---------------------------------------------------------------------------
 * Signal-deck canvas: layered oscilloscope waves + drifting packets + grid
 * ------------------------------------------------------------------------- */
const stageCanvas = ref<HTMLCanvasElement | null>(null)
let rafId = 0
let resizeObserver: ResizeObserver | null = null

const prefersReducedMotion = () =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches

interface Packet {
  wave: number
  t: number
  speed: number
}

const WAVE_CONFIG = [
  { amplitude: 0.085, frequency: 1.6, speed: 0.00042, alpha: 0.5, y: 0.42 },
  { amplitude: 0.06, frequency: 2.4, speed: -0.0003, alpha: 0.3, y: 0.55 },
  { amplitude: 0.11, frequency: 1.1, speed: 0.00022, alpha: 0.18, y: 0.68 }
]

const startSignal = (canvas: HTMLCanvasElement) => {
  const ctx = canvas.getContext('2d')
  if (!ctx) return

  let width = 0
  let height = 0
  let dpr = 1

  const packets: Packet[] = Array.from({ length: 14 }, (_, index) => ({
    wave: index % WAVE_CONFIG.length,
    t: Math.random(),
    speed: 0.0016 + Math.random() * 0.0024
  }))

  const resize = () => {
    const rect = canvas.getBoundingClientRect()
    dpr = Math.min(window.devicePixelRatio || 1, 2)
    width = Math.max(1, Math.floor(rect.width))
    height = Math.max(1, Math.floor(rect.height))
    canvas.width = width * dpr
    canvas.height = height * dpr
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
  }
  resize()

  const waveY = (cfg: (typeof WAVE_CONFIG)[number], x: number, time: number) => {
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

    /* hairline grid */
    ctx.strokeStyle = 'rgba(236, 245, 241, 0.045)'
    ctx.lineWidth = 1
    const step = 44
    ctx.beginPath()
    for (let x = step; x < width; x += step) {
      ctx.moveTo(x + 0.5, 0)
      ctx.lineTo(x + 0.5, height)
    }
    for (let y = step; y < height; y += step) {
      ctx.moveTo(0, y + 0.5)
      ctx.lineTo(width, y + 0.5)
    }
    ctx.stroke()

    /* oscilloscope waves */
    WAVE_CONFIG.forEach((cfg, waveIndex) => {
      ctx.beginPath()
      for (let x = 0; x <= width; x += 3) {
        const y = waveY(cfg, x, time + waveIndex * 4000)
        if (x === 0) ctx.moveTo(x, y)
        else ctx.lineTo(x, y)
      }
      ctx.strokeStyle = `rgba(47, 224, 168, ${cfg.alpha})`
      ctx.lineWidth = waveIndex === 0 ? 1.6 : 1
      ctx.shadowColor = 'rgba(47, 224, 168, 0.55)'
      ctx.shadowBlur = waveIndex === 0 ? 12 : 0
      ctx.stroke()
      ctx.shadowBlur = 0
    })

    /* packets traveling along wave 0 */
    for (const packet of packets) {
      packet.t += packet.speed / 10
      if (packet.t > 1) packet.t -= 1
      const cfg = WAVE_CONFIG[packet.wave]
      const x = packet.t * width
      const y = waveY(cfg, x, time + packet.wave * 4000)
      const glow = packet.wave === 0 ? 0.9 : 0.45
      ctx.beginPath()
      ctx.arc(x, y, packet.wave === 0 ? 2.2 : 1.4, 0, Math.PI * 2)
      ctx.fillStyle = `rgba(140, 245, 205, ${glow})`
      ctx.shadowColor = 'rgba(47, 224, 168, 0.9)'
      ctx.shadowBlur = 8
      ctx.fill()
      ctx.shadowBlur = 0
    }
  }

  if (prefersReducedMotion()) {
    drawFrame(1200)
    return
  }

  const loop = (time: number) => {
    drawFrame(time)
    rafId = requestAnimationFrame(loop)
  }
  rafId = requestAnimationFrame(loop)

  resizeObserver = new ResizeObserver(() => resize())
  resizeObserver.observe(canvas)
}

onMounted(() => {
  if (stageCanvas.value) startSignal(stageCanvas.value)
})

onBeforeUnmount(() => {
  cancelAnimationFrame(rafId)
  resizeObserver?.disconnect()
})
</script>

<style scoped>
.auth-shell {
  position: relative;
  display: grid;
  width: 100%;
  min-height: 100vh;
  grid-template-columns: minmax(0, 1.15fr) minmax(0, 1fr);
  color: var(--crc-text);
  background: var(--crc-canvas);
  overflow: hidden;
  transition: background-color var(--crc-duration) var(--crc-ease);
}

/* -- Signal stage (always deep-space dark) ------------------------------------ */

.auth-shell__stage {
  position: relative;
  min-height: 100vh;
  overflow: hidden;
  border-right: 1px solid var(--crc-border);
  background:
    radial-gradient(ellipse 85% 60% at 22% 12%, var(--crc-accent-soft) 0%, transparent 58%),
    radial-gradient(ellipse 65% 55% at 88% 92%, var(--crc-info-soft) 0%, transparent 62%),
    linear-gradient(155deg, var(--crc-surface) 0%, var(--crc-canvas) 55%, var(--crc-canvas) 100%);
}

.auth-shell__canvas {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
}

.auth-shell__stage-inner {
  position: relative;
  z-index: 1;
  display: flex;
  height: 100%;
  min-height: 100vh;
  padding: clamp(28px, 4.5vw, 64px);
  flex-direction: column;
  justify-content: space-between;
}

.auth-shell__brand {
  display: flex;
  align-items: center;
  gap: 12px;
}

.auth-shell__brand-copy {
  display: flex;
  flex-direction: column;
  line-height: 1.3;
}

.auth-shell__brand-copy strong {
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 15px;
  font-weight: 600;
  letter-spacing: 0;
}

.auth-shell__brand-copy span {
  margin-top: 3px;
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  font-weight: 500;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.auth-shell__headline {
  max-width: 640px;
}

.auth-shell__eyebrow {
  display: flex;
  margin: 0 0 22px;
  align-items: center;
  gap: 10px;
  color: var(--crc-accent);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.06em;
}

.auth-shell__eyebrow::before {
  content: '';
  width: 26px;
  height: 1px;
  background: var(--crc-accent);
}

.auth-shell__title {
  display: flex;
  margin: 0;
  flex-direction: column;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: clamp(44px, 6.2vw, 88px);
  font-weight: 600;
  letter-spacing: 0;
  line-height: 1.08;
  text-wrap: balance;
}

.auth-shell__title-accent {
  color: transparent;
  background: linear-gradient(100deg, var(--crc-accent) 10%, var(--crc-accent-hover) 50%, var(--crc-accent) 90%);
  background-clip: text;
  -webkit-background-clip: text;
}

.auth-shell__sub {
  margin: 26px 0 0;
  color: var(--crc-text-subtle);
  font-family: var(--crc-font-mono);
  font-size: 12px;
  letter-spacing: 0.06em;
}

.auth-shell__sub svg {
  color: var(--crc-accent);
  vertical-align: -1.5px;
}

.auth-shell__sub i {
  color: #2fe0a8;
  font-style: normal;
}

.auth-shell__telemetry {
  display: flex;
  max-width: 560px;
  flex-direction: column;
  gap: 0;
  border-top: 1px solid var(--crc-border);
}

.auth-shell__telemetry-row {
  display: flex;
  padding: 12px 0;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  border-bottom: 1px solid var(--crc-border);
  font-family: var(--crc-font-mono);
}

.auth-shell__telemetry-row span {
  color: var(--crc-text-subtle);
  font-size: 10px;
  letter-spacing: 0.06em;
}

.auth-shell__telemetry-row em {
  color: var(--crc-text);
  font-size: 11px;
  font-style: normal;
  letter-spacing: 0.06em;
}

.auth-shell__telemetry-live {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  color: var(--crc-accent) !important;
}

.auth-shell__live-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--crc-accent);
  box-shadow: 0 0 10px var(--crc-accent);
  animation: auth-live-pulse 1.8s ease-in-out infinite;
}

@keyframes auth-live-pulse {
  0%,
  100% {
    opacity: 1;
    transform: scale(1);
  }
  50% {
    opacity: 0.45;
    transform: scale(0.8);
  }
}

/* -- Form side ------------------------------------------------------------------- */

.auth-shell__side {
  position: relative;
  display: grid;
  min-height: 100vh;
  padding: 40px clamp(20px, 4vw, 56px);
  place-items: center;
}

.auth-shell__utility {
  position: absolute;
  top: 20px;
  right: 20px;
  z-index: 2;
}

@keyframes auth-panel-in {
  from {
    opacity: 0;
    transform: translateY(18px) scale(0.985);
    filter: blur(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0) scale(1);
    filter: blur(0);
  }
}

.auth-shell__panel {
  position: relative;
  z-index: 1;
  width: 100%;
  max-width: 420px;
  padding: 34px 34px 30px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-lg);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-md);
  animation: auth-panel-in 640ms var(--crc-ease-expo) 120ms both;
}

.auth-shell__panel::before {
  content: '';
  position: absolute;
  inset: -1px -1px auto;
  height: 3px;
  border-radius: var(--crc-radius-lg) var(--crc-radius-lg) 0 0;
  background: var(--crc-brand-gradient);
}

.auth-shell__panel-eyebrow {
  margin: 0 0 14px;
  color: var(--crc-accent);
  font-family: var(--crc-font-mono);
  font-size: 10px;
  font-weight: 500;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.auth-shell__header {
  margin: 0 0 26px;
}

.auth-shell__header h1 {
  margin: 0;
  color: var(--crc-text-strong);
  font-family: var(--crc-font-display);
  font-size: 28px;
  font-weight: 600;
  letter-spacing: 0;
  line-height: 1.25;
}

.auth-shell__header > p:last-child {
  margin: 10px 0 0;
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.65;
}

.auth-shell__content {
  width: 100%;
}

.auth-shell__footer {
  margin-top: 24px;
  padding-top: 18px;
  border-top: 1px solid var(--crc-border);
  color: var(--crc-text-muted);
  font-size: 12px;
  line-height: 1.6;
  text-align: center;
}

@media (max-width: 960px) {
  .auth-shell {
    grid-template-columns: 1fr;
  }

  .auth-shell__stage {
    min-height: 300px;
  }

  .auth-shell__stage-inner {
    min-height: 300px;
    padding: 24px 22px;
  }

  .auth-shell__title {
    font-size: clamp(34px, 8vw, 52px);
  }

  .auth-shell__telemetry,
  .auth-shell__sub {
    display: none;
  }

  .auth-shell__side {
    min-height: auto;
    padding: 34px 18px 44px;
    place-items: start center;
  }

  .auth-shell__panel {
    padding: 28px 24px 26px;
  }
}
</style>
