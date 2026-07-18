<template>
  <main class="auth-shell">
    <div class="auth-shell__utility">
      <ThemeSwitcher />
    </div>

    <section class="auth-shell__panel" aria-labelledby="auth-shell-title">
      <div class="auth-shell__brand" aria-label="Chat Responses Codex">
        <BrandLogo :size="40" class="auth-shell__brand-mark" />
        <div class="auth-shell__brand-copy">
          <strong>Chat Responses Codex</strong>
          <span>Gateway Console</span>
        </div>
      </div>

      <header class="auth-shell__header">
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
  </main>
</template>

<script setup lang="ts">
import ThemeSwitcher from '@/components/ThemeSwitcher.vue'
import BrandLogo from '@/components/BrandLogo.vue'

defineProps<{
  title: string
  subtitle: string
}>()
</script>

<style scoped>
.auth-shell {
  position: relative;
  display: grid;
  width: 100%;
  min-height: 100vh;
  padding: 56px 20px;
  place-items: center;
  color: var(--crc-text);
  background: var(--crc-canvas);
  overflow: hidden;
  transition: background-color var(--crc-duration) var(--crc-ease);
}

.auth-shell::before,
.auth-shell::after {
  content: '';
  position: absolute;
  width: 520px;
  height: 520px;
  border-radius: 50%;
  filter: blur(90px);
  opacity: 0.16;
  pointer-events: none;
}

.auth-shell::before {
  top: -180px;
  right: -120px;
  background: var(--crc-accent);
}

.auth-shell::after {
  bottom: -200px;
  left: -140px;
  background: var(--crc-info);
}

@keyframes auth-shell-rise {
  from {
    opacity: 0;
    transform: translateY(14px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.auth-shell__utility {
  position: absolute;
  top: 20px;
  right: 20px;
  z-index: 2;
}

.auth-shell__panel {
  position: relative;
  z-index: 1;
  width: 100%;
  max-width: 420px;
  padding: 30px 32px 28px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-lg);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-md);
  animation: auth-shell-rise var(--crc-duration-slow) var(--crc-ease-out) both;
}

.auth-shell__panel::before {
  content: '';
  position: absolute;
  inset: -1px -1px auto;
  height: 4px;
  border-radius: var(--crc-radius-lg) var(--crc-radius-lg) 0 0;
  background: var(--crc-brand-gradient);
}

.auth-shell__brand {
  display: flex;
  align-items: center;
  gap: 11px;
}

.auth-shell__brand-mark {
  width: 40px;
  height: 40px;
  flex: 0 0 40px;
  border-radius: var(--crc-radius);
}

.auth-shell__brand-copy {
  display: flex;
  flex-direction: column;
  line-height: 1.2;
}

.auth-shell__brand-copy strong {
  color: var(--crc-text-strong);
  font-size: 14px;
  font-weight: 650;
}

.auth-shell__brand-copy span {
  margin-top: 4px;
  color: var(--crc-text-muted);
  font-size: 10px;
  font-weight: 550;
  letter-spacing: 0;
  text-transform: uppercase;
}

.auth-shell__header {
  margin: 30px 0 24px;
}

.auth-shell__header h1 {
  margin: 0;
  color: var(--crc-text-strong);
  font-size: 24px;
  font-weight: 680;
  letter-spacing: 0;
  line-height: 1.3;
}

.auth-shell__header p {
  margin: 8px 0 0;
  color: var(--crc-text-muted);
  font-size: 13px;
  line-height: 1.6;
}

.auth-shell__content {
  width: 100%;
}

.auth-shell__footer {
  margin-top: 22px;
  padding-top: 18px;
  border-top: 1px solid var(--crc-border);
  color: var(--crc-text-muted);
  font-size: 12px;
  line-height: 1.6;
  text-align: center;
}

@media (max-width: 520px) {
  .auth-shell {
    padding: 70px 16px 28px;
    place-items: start center;
  }

  .auth-shell__utility {
    top: 16px;
    right: 16px;
  }

  .auth-shell__panel {
    padding: 26px 22px 24px;
  }

  .auth-shell__header {
    margin-top: 26px;
  }
}
</style>
