<template>
  <div class="announcement-page">
    <el-card shadow="hover" class="announcement-card" v-loading="loading">
      <template #header>
        <div class="page-header">
          <div>
            <h2>公告管理</h2>
            <p>管理员每次保存都会生成新的公告版本，门户用户在下一次登录或刷新时会重新确认。</p>
          </div>
          <el-button :loading="loading" @click="loadAnnouncement">重新加载</el-button>
        </div>
      </template>

      <el-alert
        title="公告默认以纯文本展示。启用后会在门户登录后弹出；关闭后会作为草稿保留，不再弹出。"
        type="info"
        :closable="false"
        show-icon
        class="announcement-note"
      />

      <el-form :model="form" label-width="120px" class="announcement-form">
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
          <span>当前版本 ID</span>
          <strong>{{ announcementId || '未发布' }}</strong>
        </div>
        <div class="meta-item">
          <span>更新时间</span>
          <strong>{{ formatUpdatedAt(updatedAt) }}</strong>
        </div>
      </div>

      <div class="announcement-actions">
        <el-button type="primary" :loading="saving" @click="handleSubmit">保存并发布</el-button>
        <el-button :disabled="saving" @click="loadAnnouncement">重置为服务端内容</el-button>
      </div>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue'
import { ElMessage } from 'element-plus'
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
  padding: 20px;
}

.announcement-card {
  max-width: 960px;
}

.page-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.page-header h2 {
  margin: 0;
}

.page-header p {
  margin: 8px 0 0;
  color: #6b7280;
  font-size: 14px;
}

.announcement-note {
  margin-bottom: 20px;
}

.announcement-form {
  margin-bottom: 20px;
}

.announcement-meta {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
  margin-bottom: 20px;
  padding: 16px;
  border-radius: 12px;
  background: #f8fafc;
  border: 1px solid #e5e7eb;
}

.meta-item {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.meta-item span {
  font-size: 12px;
  color: #6b7280;
}

.meta-item strong {
  font-size: 14px;
  color: #111827;
  word-break: break-all;
}

.announcement-actions {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
}

@media (max-width: 768px) {
  .page-header {
    flex-direction: column;
    align-items: flex-start;
  }

  .announcement-meta {
    grid-template-columns: 1fr;
  }
}
</style>
