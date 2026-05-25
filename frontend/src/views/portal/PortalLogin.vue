<template>
  <div class="portal-login-container">
    <div class="login-box">
      <div class="login-header">
        <h1>自助门户</h1>
        <p>使用工号和密钥登录</p>
      </div>

      <el-form
        :model="form"
        :rules="rules"
        ref="formRef"
        @submit.prevent="handleLogin"
        class="login-form"
      >
        <el-form-item label="工号" prop="employee_id">
          <el-input
            v-model="form.employee_id"
            placeholder="请输入工号"
            clearable
            @keyup.enter="handleLogin"
          />
        </el-form-item>

        <el-form-item label="密钥" prop="key">
          <el-input
            v-model="form.key"
            type="password"
            placeholder="请输入下游密钥"
            show-password
            clearable
            @keyup.enter="handleLogin"
          />
        </el-form-item>

        <el-form-item>
          <el-button
            type="primary"
            @click="handleLogin"
            :loading="loading"
            class="login-button"
          >
            登录
          </el-button>
        </el-form-item>
      </el-form>

      <div class="login-footer">
        <p>首次使用？请联系管理员获取工号和密钥</p>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { portalApi } from '@/api/portal'

const router = useRouter()
const loading = ref(false)
const formRef = ref()

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

    // 保存 token 到 localStorage
    localStorage.setItem('portal_token', data.token)
    localStorage.setItem('portal_employee_id', form.value.employee_id)

    ElMessage.success('登录成功')
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

<style scoped>
.portal-login-container {
  display: flex;
  justify-content: center;
  align-items: center;
  min-height: 100vh;
  background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
}

.login-box {
  background: white;
  border-radius: 8px;
  box-shadow: 0 10px 40px rgba(0, 0, 0, 0.2);
  padding: 40px;
  width: 100%;
  max-width: 400px;
}

.login-header {
  text-align: center;
  margin-bottom: 30px;
}

.login-header h1 {
  margin: 0 0 10px 0;
  font-size: 24px;
  color: #333;
}

.login-header p {
  margin: 0;
  color: #666;
  font-size: 14px;
}

.login-form {
  margin-bottom: 20px;
}

.login-button {
  width: 100%;
}

.login-footer {
  text-align: center;
  border-top: 1px solid #e0e0e0;
  padding-top: 20px;
}

.login-footer p {
  margin: 0;
  color: #999;
  font-size: 12px;
}
</style>
