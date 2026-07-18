<template>
  <AuthShell title="自助门户" subtitle="使用工号和下游密钥访问个人工作台">
    <el-form
      ref="formRef"
      class="auth-form"
      label-position="top"
      :model="form"
      :rules="rules"
      @submit.prevent="handleLogin"
    >
      <el-form-item label="工号" prop="employee_id">
        <el-input
          v-model="form.employee_id"
          autocomplete="username"
          placeholder="请输入工号"
          size="large"
          clearable
          :prefix-icon="Postcard"
        />
      </el-form-item>

      <el-form-item label="密钥" prop="key">
        <el-input
          v-model="form.key"
          autocomplete="current-password"
          type="password"
          placeholder="请输入下游密钥"
          size="large"
          show-password
          clearable
          :prefix-icon="Key"
        />
      </el-form-item>

      <el-form-item class="auth-form__action">
        <el-button
          native-type="submit"
          :type="succeeded ? 'success' : 'primary'"
          size="large"
          :loading="loading"
          class="auth-submit"
        >
          {{ succeeded ? '登录成功,正在进入…' : '登录' }}
        </el-button>
      </el-form-item>
    </el-form>

    <template #footer>首次使用请联系管理员获取工号和密钥</template>
  </AuthShell>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { Key, Postcard } from '@element-plus/icons-vue'
import AuthShell from '@/components/AuthShell.vue'
import { portalApi } from '@/api/portal'

const router = useRouter()
const loading = ref(false)
const formRef = ref()
const succeeded = ref(false)

const form = ref({
  employee_id: '',
  key: ''
})

const rules = {
  employee_id: [
    { required: true, message: '请输入工号', trigger: 'blur' }
  ],
  key: [
    { required: true, message: '请输入密钥', trigger: 'blur' }
  ]
}

const handleLogin = async () => {
  try {
    await formRef.value.validate()
    loading.value = true

    const { data } = await portalApi.login({
      employee_id: form.value.employee_id,
      key: form.value.key
    })

    localStorage.setItem('portal_token', data.token)
    localStorage.setItem('portal_employee_id', form.value.employee_id)

    succeeded.value = true
    ElMessage.success('登录成功')
    await new Promise(resolve => setTimeout(resolve, 550))
    router.push('/portal')
  } catch (error: any) {
    if (error.response?.status === 401) {
      ElMessage.error('工号或密钥错误')
    } else {
      ElMessage.error('登录失败，请重试')
    }
  } finally {
    loading.value = false
  }
}
</script>
