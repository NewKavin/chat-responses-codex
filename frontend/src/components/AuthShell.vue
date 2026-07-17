<template>
  <main class="auth-shell">
    <div class="auth-shell__utility">
      <ThemeSwitcher />
    </div>

    <section class="auth-shell__panel" aria-labelledby="auth-shell-title">
      <div class="auth-shell__brand" aria-label="Chat Responses Codex">
        <div class="auth-shell__brand-mark" aria-hidden="true">C</div>
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
}

.auth-shell__utility {
  position: absolute;
  top: 20px;
  right: 20px;
}

.auth-shell__panel {
  width: 100%;
  max-width: 420px;
  padding: 30px 32px 28px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-lg);
  background: var(--crc-surface);
}

.auth-shell__brand {
  display: flex;
  align-items: center;
  gap: 11px;
}

.auth-shell__brand-mark {
  display: grid;
  width: 38px;
  height: 38px;
  flex: 0 0 38px;
  place-items: center;
  border-radius: var(--crc-radius);
  color: var(--crc-surface);
  background: var(--crc-accent);
  font-size: 18px;
  font-weight: 750;
  letter-spacing: -0.04em;
}

html.dark .auth-shell__brand-mark {
  color: var(--crc-canvas);
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
  letter-spacing: 0.08em;
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
  letter-spacing: -0.025em;
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
