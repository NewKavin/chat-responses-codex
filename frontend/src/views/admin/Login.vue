<template>
  <AuthShell title="管理员登录" subtitle="使用管理账号进入网关控制台">
    <el-form
      ref="formRef"
      class="auth-form"
      label-position="top"
      :model="form"
      :rules="rules"
      @submit.prevent="handleLogin"
    >
      <el-form-item label="用户名" prop="username">
        <el-input
          v-model="form.username"
          autocomplete="username"
          placeholder="请输入用户名"
          size="large"
          :prefix-icon="User"
        />
      </el-form-item>

      <el-form-item label="密码" prop="password">
        <el-input
          v-model="form.password"
          autocomplete="current-password"
          type="password"
          placeholder="请输入密码"
          size="large"
          show-password
          :prefix-icon="Lock"
        />
      </el-form-item>

      <el-form-item class="auth-form__action">
        <el-button
          native-type="submit"
          type="primary"
          size="large"
          :loading="loading"
          class="auth-submit"
        >
          登录
        </el-button>
      </el-form-item>
    </el-form>

    <template #footer>仅限已授权的系统管理员使用</template>
  </AuthShell>
</template>

<script setup lang="ts">
import { reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { Lock, User } from '@element-plus/icons-vue'
import AuthShell from '@/components/AuthShell.vue'
import { adminApi, hasUsableAdminToken } from '@/api/admin'
import { useAuthStore } from '@/stores/auth'

const router = useRouter()
const authStore = useAuthStore()
const formRef = ref()
const loading = ref(false)

const form = reactive({
  username: '',
  password: ''
})

const rules = {
  username: [{ required: true, message: '请输入用户名', trigger: 'blur' }],
  password: [{ required: true, message: '请输入密码', trigger: 'blur' }]
}

const handleLogin = async () => {
  try {
    await formRef.value.validate()
    loading.value = true

    const response = await adminApi.login(form)
    const { data } = response

    if (response.status !== 200 || !hasUsableAdminToken(data.token)) {
      throw new Error('INVALID_LOGIN_RESPONSE')
    }

    authStore.setToken(data.token)
    ElMessage.success('登录成功')
    router.push('/admin')
  } catch (error: any) {
    if (error.response?.status === 401) {
      ElMessage.error('用户名或密码错误')
    } else {
      ElMessage.error('登录失败，请稍后重试')
    }
  } finally {
    loading.value = false
  }
}
</script>
