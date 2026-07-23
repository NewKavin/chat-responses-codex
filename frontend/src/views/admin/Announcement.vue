<template>
  <div class="crc-page announcement-page">
    <header class="crc-page-header">
      <div>
        <p class="crc-eyebrow">BROADCAST // NOTICE</p>
        <h1 class="crc-page-title">公告管理</h1>
        <p class="crc-page-description">每次保存都会生成新版本，门户用户在下次登录或刷新时重新确认。</p>
      </div>
      <el-button :icon="RefreshCw" :loading="loading" @click="loadAnnouncement">重新加载</el-button>
    </header>

    <section v-loading="loading" class="announcement-form-surface">
      <div class="announcement-surface-head">
        <span class="announcement-surface-head__icon" aria-hidden="true">
          <Megaphone :size="18" :stroke-width="1.8" />
        </span>
        <div class="announcement-surface-head__text">
          <span class="announcement-surface-head__label">DRAFT EDITOR</span>
          <strong class="announcement-surface-head__title">公告草稿编辑器</strong>
        </div>
        <span class="announcement-surface-head__status" :class="{ 'is-live': form.active }">
          <span class="crc-pulse-dot" :class="form.active ? 'crc-pulse-dot--success' : 'crc-pulse-dot--muted'" aria-hidden="true"></span>
          {{ form.active ? '已启用' : '草稿' }}
        </span>
      </div>
      <el-alert
        title="公告默认以纯文本展示。启用后会在门户登录后弹出；关闭后会作为草稿保留，不再弹出。"
        type="info"
        :closable="false"
        show-icon
        class="announcement-note"
      />

      <el-form :model="form" label-position="top" class="announcement-form">
        <el-form-item label="标题">
          <el-input
            v-model="form.title"
            maxlength="120"
            show-word-limit
            placeholder="请输入公告标题"
          />
        </el-form-item>

        <el-form-item label="正文">
          <el-input
            v-model="form.content"
            type="textarea"
            :rows="10"
            maxlength="5000"
            show-word-limit
            placeholder="请输入公告正文"
          />
        </el-form-item>

        <el-form-item label="等级">
          <el-radio-group v-model="form.level">
            <el-radio-button
              v-for="option in levelOptions"
              :key="option.value"
              :label="option.value"
              :value="option.value"
            >
              {{ option.label }}
            </el-radio-button>
          </el-radio-group>
        </el-form-item>

        <el-form-item label="启用">
          <el-switch v-model="form.active" />
        </el-form-item>
      </el-form>

      <div class="announcement-meta">
        <div class="meta-item">
          <span class="meta-item__label">VERSION ID // 当前版本</span>
          <strong class="meta-item__value crc-mono">{{ announcementId || '未发布' }}</strong>
        </div>
        <div class="meta-item">
          <span class="meta-item__label">UPDATED // 更新时间</span>
          <strong class="meta-item__value">{{ formatUpdatedAt(updatedAt) }}</strong>
        </div>
      </div>

      <div class="announcement-actions">
        <el-button type="primary" :loading="saving" @click="handleSubmit">保存并发布</el-button>
        <el-button :disabled="saving" @click="loadAnnouncement">重置为服务端内容</el-button>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { Megaphone, RefreshCw } from '@lucide/vue'
import { adminApi } from '@/api/admin'
import type { AnnouncementLevel } from '@/types'

type AnnouncementForm = {
  title: string
  content: string
  level: AnnouncementLevel
  active: boolean
}

type ApiError = {
  response?: {
    data?: {
      error?: {
        message?: string
      }
      message?: string
    }
  }
}

const levelOptions: Array<{ label: string; value: AnnouncementLevel }> = [
  { label: '信息', value: 'info' },
  { label: '成功', value: 'success' },
  { label: '警告', value: 'warning' },
  { label: '错误', value: 'error' }
]

const loading = ref(false)
const saving = ref(false)
const announcementId = ref('')
const updatedAt = ref(0)

const form = reactive<AnnouncementForm>({
  title: '',
  content: '',
  level: 'info',
  active: false
})

const resetForm = () => {
  form.title = ''
  form.content = ''
  form.level = 'info'
  form.active = false
  announcementId.value = ''
  updatedAt.value = 0
}

const formatUpdatedAt = (timestamp: number) => {
  if (!timestamp) {
    return '未更新'
  }
  return new Date(timestamp * 1000).toLocaleString('zh-CN', {
    hour12: false
  })
}

const extractErrorMessage = (error: unknown, fallback: string) => {
  const apiError = error as ApiError
  if (apiError.response?.data?.error?.message) {
    return apiError.response.data.error.message
  }
  if (apiError.response?.data?.message) {
    return apiError.response.data.message
  }
  if (error instanceof Error && error.message) {
    return error.message
  }
  return fallback
}

const loadAnnouncement = async () => {
  try {
    loading.value = true
    const { data } = await adminApi.getAnnouncement()
    const announcement = data.announcement

    if (!announcement) {
      resetForm()
      return
    }

    form.title = announcement.title
    form.content = announcement.content
    form.level = announcement.level
    form.active = announcement.active
    announcementId.value = announcement.id
    updatedAt.value = announcement.updated_at
  } catch (error) {
    ElMessage.error(extractErrorMessage(error, '加载公告失败'))
  } finally {
    loading.value = false
  }
}

const handleSubmit = async () => {
  const title = form.title.trim()
  const content = form.content.trim()

  if (title.length > 120) {
    ElMessage.error('标题最长 120 个字符')
    return
  }

  if (content.length > 5000) {
    ElMessage.error('正文最长 5000 个字符')
    return
  }

  if (form.active && (!title || !content)) {
    ElMessage.error('启用状态下标题和正文不能为空')
    return
  }

  try {
    saving.value = true
    const { data } = await adminApi.updateAnnouncement({
      title,
      content,
      level: form.level,
      active: form.active
    })

    const announcement = data.announcement
    if (!announcement) {
      throw new Error('公告保存响应缺少内容')
    }

    form.title = announcement.title
    form.content = announcement.content
    form.level = announcement.level
    form.active = announcement.active
    announcementId.value = announcement.id
    updatedAt.value = announcement.updated_at
    ElMessage.success(form.active ? '公告已发布' : '公告草稿已保存')
  } catch (error) {
    ElMessage.error(extractErrorMessage(error, '保存公告失败'))
  } finally {
    saving.value = false
  }
}

onMounted(() => {
  loadAnnouncement()
})
</script>

<style scoped>
.announcement-page {
  min-height: 100%;
}

.announcement-form-surface {
  position: relative;
  width: 100%;
  max-width: 760px;
  padding: 24px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius-lg, 16px);
  background: var(--crc-surface);
  box-shadow: var(--crc-shadow-xs);
  overflow: hidden;
}

.announcement-form-surface::before {
  content: '';
  position: absolute;
  inset: 0 0 auto;
  height: 2px;
  background: linear-gradient(90deg, var(--crc-accent), transparent 65%);
  opacity: 0.7;
  pointer-events: none;
}

.announcement-surface-head {
  display: flex;
  align-items: center;
  gap: 14px;
  margin-bottom: 20px;
  padding-bottom: 16px;
  border-bottom: 1px dashed var(--crc-border);
}

.announcement-surface-head__icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
  border-radius: 12px;
  color: var(--crc-accent-ink, var(--crc-accent));
  background: var(--crc-accent-soft, rgba(10, 143, 111, 0.1));
  border: 1px solid var(--crc-border);
  flex-shrink: 0;
}

.announcement-surface-head__text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.announcement-surface-head__label {
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.14em;
  color: var(--crc-text-muted);
}

.announcement-surface-head__title {
  font-family: var(--crc-font-display);
  font-size: 16px;
  font-weight: 600;
  color: var(--crc-text-strong);
}

.announcement-surface-head__status {
  margin-left: auto;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border: 1px solid var(--crc-border);
  border-radius: 999px;
  background: var(--crc-surface-muted);
  font-family: var(--crc-font-mono);
  font-size: 11px;
  color: var(--crc-text-muted);
  white-space: nowrap;
}

.announcement-surface-head__status.is-live {
  color: var(--crc-accent-ink, var(--crc-accent));
  border-color: var(--crc-accent);
  background: var(--crc-accent-soft, rgba(10, 143, 111, 0.08));
}

.announcement-note {
  margin-bottom: 20px;
}

.announcement-form {
  margin-bottom: 20px;
}

.announcement-form :deep(.el-form-item__label) {
  font-family: var(--crc-font-mono);
  font-size: 11px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--crc-text-muted);
}

.announcement-meta {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
  margin-bottom: 20px;
  padding: 16px;
  border: 1px solid var(--crc-border);
  border-radius: var(--crc-radius);
  background: var(--crc-surface-muted);
}

.meta-item {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
}

.meta-item__label {
  font-family: var(--crc-font-mono);
  font-size: 10px;
  letter-spacing: 0.14em;
  color: var(--crc-text-muted);
}

.meta-item__value {
  font-family: var(--crc-font-display);
  color: var(--crc-text-strong);
  font-size: 15px;
  font-weight: 600;
  word-break: break-all;
}

.announcement-actions {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
}

@media (max-width: 767px) {
  .announcement-meta {
    grid-template-columns: 1fr;
  }

  .announcement-actions {
    align-items: stretch;
    flex-direction: column;
  }

  .announcement-actions .el-button {
    margin-left: 0;
  }
}
</style>
