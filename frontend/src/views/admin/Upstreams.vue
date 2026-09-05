<template>
  <div class="crc-page upstreams-page">
    <header class="crc-page-header">
      <div>
        <p class="crc-eyebrow">PROVIDERS // UPSTREAMS</p>
        <h1 class="crc-page-title">上游管理</h1>
        <p class="crc-page-description">配置模型供应方、协议、密钥、上下文限制和智能路由策略。</p>
      </div>
      <div class="upstream-batch-actions">
        <el-button
          :disabled="selectedUpstreams.length === 0"
          @click="handleBatchToggle(true)"
        >
          <CircleCheck :size="15" :stroke-width="2" style="margin-right: 5px" />
          批量启用<template v-if="selectedUpstreams.length">（{{ selectedUpstreams.length }}）</template>
        </el-button>
        <el-button
          :disabled="selectedUpstreams.length === 0"
          @click="handleBatchToggle(false)"
        >
          <CircleSlash :size="15" :stroke-width="2" style="margin-right: 5px" />
          批量禁用<template v-if="selectedUpstreams.length">（{{ selectedUpstreams.length }}）</template>
        </el-button>
        <el-button
          type="danger"
          :disabled="selectedUpstreams.length === 0"
          @click="handleBatchDelete"
        >
          <Trash2 :size="15" :stroke-width="2" style="margin-right: 5px" />
          批量删除<template v-if="selectedUpstreams.length">（{{ selectedUpstreams.length }}）</template>
        </el-button>
        <el-button
          :disabled="selectedUpstreams.length === 0"
          @click="openBatchUpdate"
        >
          <Settings2 :size="15" :stroke-width="2" style="margin-right: 5px" />
          批量修改字段<template v-if="selectedUpstreams.length">（{{ selectedUpstreams.length }}）</template>
        </el-button>
        <el-button type="primary" @click="handleCreate">
          <Plus :size="15" :stroke-width="2" style="margin-right: 5px" />创建上游
        </el-button>
        <el-button :loading="globalFetching" @click="openGlobalFetch">
          <RefreshCw :size="15" :stroke-width="2" style="margin-right: 5px" />
          全局获取模型
        </el-button>
      </div>
    </header>

    <el-form :inline="true" class="crc-toolbar upstream-filters">
      <el-form-item>
        <template #label><span class="filter-label"><Activity :size="12" :stroke-width="2" />状态</span></template>
        <el-select v-model="filters.status" placeholder="全部">
          <el-option label="全部" value="all" />
          <el-option label="启用" value="active" />
          <el-option label="禁用" value="inactive" />
        </el-select>
      </el-form-item>
      <el-form-item>
        <template #label><span class="filter-label"><PlugZap :size="12" :stroke-width="2" />协议</span></template>
        <el-select v-model="filters.protocol" placeholder="全部">
          <el-option label="全部" value="all" />
          <el-option
            v-for="protocol in availableProtocols"
            :key="protocol"
            :label="protocol"
            :value="protocol"
          />
        </el-select>
      </el-form-item>
      <el-form-item>
        <template #label><span class="filter-label"><CircleSlash :size="12" :stroke-width="2" />凭证</span></template>
        <el-select v-model="filters.credentials" clearable placeholder="凭证状态">
          <el-option label="凭证失败" value="failing" />
        </el-select>
      </el-form-item>
      <el-form-item>
        <template #label><span class="filter-label"><Search :size="12" :stroke-width="2" />搜索</span></template>
        <el-input v-model="filters.search" placeholder="名称 / ID / Base URL" clearable />
      </el-form-item>
      <el-form-item class="table-column-settings-item">
        <TableColumnSettings
          v-model="visibleColumnKeys"
          :columns="tableColumns"
          :default-keys="defaultColumnKeys"
        />
      </el-form-item>
    </el-form>

    <div class="crc-table-shell">
      <el-table :data="pagedUpstreams" v-loading="loading" stripe style="width: 100%"
        empty-text="当前筛选条件下暂无上游" @selection-change="handleSelectionChange">
        <el-table-column type="selection" width="48" />
        <el-table-column v-if="isColumnVisible('id')" label="ID" width="72" align="center">
          <template #default="{ $index }">{{ $index + 1 }}</template>
        </el-table-column>
        <el-table-column v-if="isColumnVisible('name')" prop="name" label="名称" min-width="200" />
        <el-table-column v-if="isColumnVisible('protocol')" label="协议" min-width="240">
          <template #default="{ row }">
            <div class="protocol-cell">
              <el-tag
                v-for="protocol in displayProtocols(row)"
                :key="`${row.id}-${protocol}`"
                size="small"
              >
                {{ protocol }}
              </el-tag>
            </div>
          </template>
        </el-table-column>
        <el-table-column
          v-if="isColumnVisible('base_url')"
          prop="base_url"
          label="Base URL"
          min-width="240"
          show-overflow-tooltip
        >
          <template #default="{ row }">
            <code class="base-url-cell">{{ row.base_url }}</code>
          </template>
        </el-table-column>
        <el-table-column v-if="isColumnVisible('models')" label="模型数量" width="100">
          <template #default="{ row }">
            {{ row.supported_models.length }}
          </template>
        </el-table-column>
        <el-table-column
          v-if="isColumnVisible('supported_models')"
          label="支持的模型"
          min-width="260"
        >
          <template #default="{ row }">
            <el-tooltip
              :content="formatModelList(row.supported_models)"
              placement="top"
              :disabled="!row.supported_models.length"
            >
              <span class="model-list-cell">{{ formatModelList(row.supported_models) }}</span>
            </el-tooltip>
          </template>
        </el-table-column>
        <el-table-column v-if="isColumnVisible('keys')" label="Key 数量" width="100">
          <template #default="{ row }">
            {{ displayKeyCount(row) }} 个
          </template>
        </el-table-column>
        <el-table-column
          v-if="isColumnVisible('key_concurrency')"
          prop="max_concurrency"
          label="每 Key 最大并发"
          width="140"
          align="center"
        >
          <template #default="{ row }">{{ row.max_concurrency }}</template>
        </el-table-column>
        <el-table-column v-if="isColumnVisible('compatibility')" label="兼容清理" width="110">
          <template #default="{ row }">
            <el-tag v-if="normalizeNonstandardPolicy(row.strip_nonstandard_chat_fields) === 'always_strip'" type="success" size="small">强制</el-tag>
            <el-tag v-else-if="normalizeNonstandardPolicy(row.strip_nonstandard_chat_fields) === 'forward'" type="warning" size="small">透传</el-tag>
            <el-tag v-else-if="normalizeNonstandardPolicy(row.strip_nonstandard_chat_fields) === 'auto' && isAutoChatCompatibility(row)" type="info" size="small">自动</el-tag>
            <span v-else>-</span>
            <el-tag v-if="row.dialect_preset" type="primary" size="small" style="margin-left: 4px">{{ row.dialect_preset }}</el-tag>
          </template>
        </el-table-column>
        
        
        <el-table-column v-if="isColumnVisible('status')" label="状态" width="100">
          <template #default="{ row }">
            <el-tag :type="row.active ? 'success' : 'danger'">
              {{ row.active ? '启用' : '禁用' }}
            </el-tag>
          </template>
        </el-table-column>

        <el-table-column v-if="isColumnVisible('route_health')" label="路由健康" min-width="180">
          <template #default="{ row }">
            <el-tooltip
              :content="formatRouteFailureClasses(row.route_health)"
              :disabled="!formatRouteFailureClasses(row.route_health)"
              placement="top"
            >
              <span>
                <el-tag type="warning" size="small">
                  冷却 {{ row.route_health?.cooldown_routes ?? 0 }}
                </el-tag>
                <span v-if="formatRouteCooldown(row.route_health)">
                  {{ formatRouteCooldown(row.route_health) }}
                </span>
              </span>
            </el-tooltip>
          </template>
        </el-table-column>

        <el-table-column v-if="isColumnVisible('concurrency_gate')" label="并发闸门" min-width="170">
          <template #default="{ row }">
            <el-tooltip
              :content="formatRuntimeStateDetail(row)"
              :disabled="!formatRuntimeStateDetail(row)"
              placement="top"
            >
              <span>
                <el-tag size="small" :type="row.runtime_state?.in_flight ? 'primary' : 'info'">
                  在途 {{ row.runtime_state?.in_flight ?? 0 }}
                </el-tag>
                <el-tag
                  v-if="(row.runtime_state?.queue_depth ?? 0) > 0"
                  size="small"
                  type="warning"
                >
                  排队 {{ row.runtime_state?.queue_depth }}
                </el-tag>
                <span
                  v-if="row.runtime_state?.queue_depth_supported === false"
                  class="muted"
                >
                  排队：本后端不支持
                </span>
                <el-tag
                  v-if="(row.runtime_state?.stale_lease_count ?? 0) > 0"
                  size="small"
                  type="danger"
                >
                  陈旧 {{ row.runtime_state?.stale_lease_count }}
                </el-tag>
                <span v-if="!row.runtime_state" class="muted">-</span>
              </span>
            </el-tooltip>
          </template>
        </el-table-column>

        <el-table-column v-if="isColumnVisible('priority')" label="优先级/权重" width="150" align="center">
          <template #default="{ row }">
            <el-input-number
              v-model="row.priority"
              :min="0"
              :max="1000"
              :step="1"
              controls-position="right"
              size="small"
              :disabled="isInlineSaving(row.id, 'priority')"
              @change="updateInlinePriority(row)"
            />
          </template>
        </el-table-column>

        <el-table-column v-if="isColumnVisible('remark')" label="备注" min-width="180" show-overflow-tooltip>
          <template #default="{ row }">{{ row.remark || '-' }}</template>
        </el-table-column>
        
        <el-table-column label="操作" width="450" fixed="right">
          <template #default="{ row }">
            <el-button size="small" @click="handleEdit(row)">编辑</el-button>
            <el-button size="small" @click="handleCopy(row)">复制</el-button>
            <el-button size="small" @click="handleToggle(row)">
              {{ row.active ? '禁用' : '启用' }}
            </el-button>
            <el-button
              size="small"
              :icon="RefreshCw"
              @click="handleResetRouteHealth(row)"
            >
              解除冷却
            </el-button>
            <el-button size="small" type="warning" @click="handleResetConcurrency(row)">
              重置并发
            </el-button>
            <el-button size="small" type="danger" @click="handleDelete(row)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>

      <div v-if="filteredUpstreams.length > 0" class="upstream-table-pagination">
        <el-pagination
          v-model:current-page="upstreamPage"
          v-model:page-size="upstreamPageSize"
          :total="filteredUpstreams.length"
          :page-sizes="[10, 20, 50, 100]"
          layout="total, sizes, prev, pager, next"
          background
        />
      </div>
    </div>
    
    <!-- Create/Edit Drawer -->
    <el-drawer
      v-model="dialogVisible"
      :title="dialogMode === 'create' ? '创建上游' : '编辑上游'"
      direction="rtl"
      size="var(--account-drawer-width)"
      :destroy-on-close="false"
      class="form-drawer upstream-account-drawer"
    >
      <el-form ref="formRef" :model="form" :rules="rules" label-position="top" class="drawer-form">
        <el-form-item v-if="dialogMode === 'edit'" label="ID">
          <el-input v-model="form.id" disabled />
        </el-form-item>
        <el-form-item label="名称" prop="name">
          <el-input v-model="form.name" placeholder="例如: OpenAI 主上游" />
        </el-form-item>
        <el-form-item label="备注">
          <el-input
            v-model="form.remark"
            type="textarea"
            :rows="2"
            placeholder="例如: 共享账号、区域或维护说明"
          />
        </el-form-item>
        <el-form-item label="续传兼容组">
          <el-input
            v-model="form.continuation_provider_group"
            clearable
            maxlength="128"
            placeholder="留空时按 Base URL 和模型自动分组"
          />
        </el-form-item>
        <el-form-item label="Base URL" prop="base_url">
          <el-input v-model="form.base_url" placeholder="https://api.openai.com" />
        </el-form-item>
        <el-form-item label="API Key" prop="api_key">
          <el-input
            v-model="form.api_key"
            type="textarea"
            :rows="3"
            placeholder="每行一个 Key&#10;支持多 Key 快速创建多个同名上游"
          />
          <span class="form-hint">多行输入多个 Key，每行一个；单 Key 时不影响原有行为</span>
        </el-form-item>
        <el-form-item label="每 Key 最大并发" prop="max_concurrency">
          <el-input-number
            v-model="form.max_concurrency"
            :min="1"
            :max="4294967295"
            :step="1"
            controls-position="right"
          />
        </el-form-item>
        <el-form-item label="协议" prop="protocols">
          <el-select v-model="form.protocols" multiple>
            <el-option label="ChatCompletions" value="ChatCompletions" />
            <el-option label="Responses" value="Responses" />
          </el-select>
        </el-form-item>
        <el-form-item label="兼容清理">
          <el-select v-model="form.strip_nonstandard_chat_fields" style="width: 220px">
            <el-option label="自动（默认）" value="auto" />
            <el-option label="强制清理" value="always_strip" />
            <el-option label="透传" value="forward" />
          </el-select>
          <span class="form-hint">自动：有探测档案时按档案处理，无档案时保守清理扩展字段；强制：始终清理；透传：原样转发</span>
        </el-form-item>
        <el-form-item label="方言预设">
          <el-select v-model="form.dialect_preset" clearable placeholder="无（按探测/Auto 兜底）" style="width: 220px">
            <el-option label="OpenAI 兼容" value="openai" />
            <el-option label="DeepSeek" value="deepseek" />
            <el-option label="GLM" value="glm" />
            <el-option label="MiniMax" value="minimax" />
            <el-option label="严格模式" value="generic-strict" />
          </el-select>
          <span class="form-hint">无探测档案时按预设静态兜底各字段处理（deepseek：reasoning_effort 直传；glm：thinking 对象值；严格模式：全剥离）</span>
        </el-form-item>
        <el-form-item label="按模型方言预设">
          <div style="width: 100%">
            <div
              v-for="(preset, pattern, index) in form.model_dialect_presets || {}"
              :key="index"
              style="display: flex; gap: 8px; margin-bottom: 8px; align-items: center"
            >
              <el-input
                :model-value="pattern"
                @update:model-value="renameModelPreset(pattern, $event)"
                placeholder="模型前缀，如 glm-* / deepseek-*"
                style="width: 260px"
              />
              <el-select :model-value="preset" @update:model-value="updateModelPreset(pattern, $event)" clearable placeholder="预设" style="width: 200px">
                <el-option label="OpenAI 兼容" value="openai" />
                <el-option label="DeepSeek" value="deepseek" />
                <el-option label="GLM" value="glm" />
                <el-option label="MiniMax" value="minimax" />
                <el-option label="严格模式" value="generic-strict" />
              </el-select>
              <el-button type="danger" :icon="Trash2" circle @click="removeModelPreset(pattern)" />
            </div>
            <el-button type="primary" plain size="small" :icon="Plus" @click="addModelPreset">添加模型预设</el-button>
            <span class="form-hint" style="display: block; margin-top: 4px">按模型 slug 覆盖方言预设（支持 glm-* 前缀通配）；比「方言预设」优先，但探测档案始终优先</span>
          </div>
        </el-form-item>

        <!-- 模型配置 -->
        <el-divider class="drawer-section">模型配置</el-divider>
        <el-form-item label="支持的模型">
          <div class="model-input-group">
            <el-select v-model="form.supported_models" multiple filterable allow-create placeholder="手动输入或点击获取模型">
              <el-option
                v-for="model in selectableModelOptions"
                :key="model"
                :label="model"
                :value="model"
              />
            </el-select>
            <el-button
              :disabled="!form.base_url || !form.api_key"
              @click="fetchModels"
              :loading="fetchingModels"
              :icon="RefreshCw"
              class="fetch-btn"
            >
              获取模型列表
            </el-button>
          </div>
        </el-form-item>


        <el-divider class="drawer-section">模型上下文</el-divider>
        <el-tabs v-model="contextConfigTab">
          <el-tab-pane label="默认上下文" name="default">
            <el-form-item label="上下文上限">
              <el-input-number v-model="form.default_model_context!.context_limit" :min="0" :max="2000000" />
              <span class="form-hint">留空或 0 表示不启用默认值，后续仅按模型覆盖配置生效</span>
            </el-form-item>
            <el-form-item label="输出预留">
              <el-input-number v-model="form.default_model_context!.output_reserve" :min="0" :max="2000000" />
              <span class="form-hint">输入 0 时自动回退到网关默认预留值</span>
            </el-form-item>
            <el-form-item label="最大输出">
              <el-input-number v-model="form.default_model_context!.max_output_tokens" :min="0" :max="2000000" />
              <span class="form-hint">对 max_tokens 做上限裁剪，0 表示不限制。可避免请求超出上游额度或模型能力</span>
            </el-form-item>
            <el-form-item label="上下文分组">
              <el-input v-model="form.default_model_context!.context_group" placeholder="可选: 与模型分组一致时可自动切换更大上下文模型" />
            </el-form-item>
            <el-form-item>
              <el-button v-if="dialogMode === 'edit'" size="small" @click="clearDefaultContextConfig">清空默认上下文</el-button>
            </el-form-item>
          </el-tab-pane>
          <el-tab-pane label="模型覆盖" name="overrides">
            <el-table :data="form.model_contexts" style="width: 100%; margin-bottom: 10px">
              <el-table-column label="模型" width="220">
                <template #default="{ row }">
                  <el-select v-model="row.slug" placeholder="选择模型" filterable allow-create>
                    <el-option v-for="model in availableModelOptions" :key="model" :label="model" :value="model" />
                  </el-select>
                </template>
              </el-table-column>
              <el-table-column label="上下文上限" width="160">
                <template #default="{ row }">
                  <el-input-number v-model="row.context_limit" :min="1" :max="2000000" />
                </template>
              </el-table-column>
              <el-table-column label="输出预留" width="160">
                <template #default="{ row }">
                  <el-input-number v-model="row.output_reserve" :min="0" :max="2000000" />
                </template>
              </el-table-column>
              <el-table-column label="最大输出" width="160">
                <template #default="{ row }">
                  <el-input-number v-model="row.max_output_tokens" :min="0" :max="2000000" />
                </template>
              </el-table-column>
              <el-table-column label="上下文分组" min-width="180">
                <template #default="{ row }">
                  <el-input v-model="row.context_group" placeholder="可选: 同组可自动切换更大上下文模型" />
                </template>
              </el-table-column>
              <el-table-column label="操作" width="100">
                <template #default="{ row }">
                  <el-button size="small" type="danger" @click="removeModelContext(row)">删除</el-button>
                </template>
              </el-table-column>
            </el-table>
            <el-button @click="addModelContext" size="small">添加模型上下文</el-button>
          </el-tab-pane>
        </el-tabs>

        <!-- 路由权重配置 -->
        <el-divider class="drawer-section">智能路由配置</el-divider>
        <el-form-item label="优先级权重">
          <el-input-number v-model="form.priority" :min="0" :max="1000" placeholder="数字越大优先级越高" />
          <el-alert
            title="说明"
            type="info"
            :closable="false"
            class="helper-text"
          >
            用于控制路由优先级。权重高的账号优先被选中。默认为0。
          </el-alert>
        </el-form-item>

        <el-form-item label="启用">
          <el-switch v-model="form.active" />
        </el-form-item>
      </el-form>
      
      <template #footer>
        <div class="drawer-footer">
          <el-button @click="dialogVisible = false">取消</el-button>
          <el-button type="primary" @click="handleSubmit" :loading="submitting">确定</el-button>
        </div>
      </template>
    </el-drawer>

    <!-- Batch update upstream fields (C6) -->
    <el-dialog
      v-model="batchUpdateVisible"
      title="批量修改上游字段"
      width="480px"
      :close-on-click-modal="false"
      @closed="resetBatchUpdateForm"
    >
      <p class="batch-update-hint">
        将对选中的 {{ selectedUpstreams.length }} 个上游应用以下字段（不填的字段保持不变）。
      </p>
      <el-form label-position="top">
        <el-form-item label="每 Key 最大并发">
          <el-input-number v-model="batchUpdateForm.max_concurrency" :min="1" :max="1000" controls-position="right" />
          <span class="batch-update-clear" @click="batchUpdateForm.max_concurrency = undefined">清除</span>
        </el-form-item>
        <el-form-item label="优先级">
          <el-input-number v-model="batchUpdateForm.priority" :min="0" :max="1000" controls-position="right" />
          <span class="batch-update-clear" @click="batchUpdateForm.priority = undefined">清除</span>
        </el-form-item>
        <el-form-item label="启用状态">
          <el-radio-group v-model="batchUpdateForm.active">
            <el-radio :label="'keep'">保持不变</el-radio>
            <el-radio :label="'true'">启用</el-radio>
            <el-radio :label="'false'">禁用</el-radio>
          </el-radio-group>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="batchUpdateVisible = false">取消</el-button>
        <el-button type="primary" :loading="batchUpdating" @click="submitBatchUpdate">
          提交修改
        </el-button>
      </template>
    </el-dialog>

    <!-- Global model discovery dialog -->
    <el-dialog
      v-model="globalFetchVisible"
      title="全局获取模型"
      width="min(720px, calc(100vw - 32px))"
      :close-on-click-modal="false"
      @closed="resetGlobalFetch"
    >
      <template v-if="globalFetching">
        <div class="global-fetch-progress">
          <el-icon class="is-loading" :size="18"><RefreshCw /></el-icon>
          <span>正在获取各上游账号的模型列表（{{ globalProgress.done }}/{{ globalProgress.total }}）…</span>
        </div>
      </template>

      <template v-else-if="globalModelPool.length > 0">
        <div class="global-fetch-summary">
          已从 <strong>{{ Object.keys(globalUpstreamModels).length }}</strong> 个上游账号获取到
          <strong>{{ globalModelPool.length }}</strong> 个模型。
          勾选模型后，凡是支持该模型的上游账号都会被自动选中并保存。
        </div>

        <!-- 批量操作工具栏 -->
        <div class="global-fetch-toolbar">
          <el-button size="small" @click="selectAllModels" data-test="select-all-button">
            全选 ({{ globalModelPool.length }})
          </el-button>
          <el-button size="small" @click="invertSelection" data-test="invert-selection-button">
            反选
          </el-button>
          <el-button size="small" @click="clearSelection" data-test="clear-selection-button">
            清空
          </el-button>

          <el-divider direction="vertical" />

          <el-input
            v-model="globalFilterText"
            size="small"
            clearable
            placeholder="筛选模型（支持正则）"
            style="width: 200px"
            data-test="global-model-filter"
          />

          <el-divider direction="vertical" />

          <span style="color: var(--el-text-color-secondary); font-size: 13px;">预设组合：</span>
          <el-button
            v-for="group in builtinModelGroups"
            :key="group.id"
            size="small"
            plain
            @click="applyModelGroup(group)"
            :data-test="`model-group-${group.id}`"
          >
            {{ group.name }}
          </el-button>
        </div>

        <el-form label-width="90px" class="global-model-form">
          <el-form-item label="选择模型">
            <el-select
              v-model="globalSelectedModels"
              multiple
              filterable
              collapse-tags
              collapse-tags-tooltip
              placeholder="搜索并勾选要全局启用的模型"
              style="width: 100%"
            >
              <el-option
                v-for="model in filteredModels"
                :key="model"
                :label="`${model}（${globalModelAccountCount(model)} 个账号支持）`"
                :value="model"
              />
            </el-select>
          </el-form-item>
        </el-form>

        <div class="global-apply-preview" v-if="globalSelectedModels.length > 0">
          <p class="global-apply-title">应用预览：以下 {{ globalSelectedModels.length }} 个模型将被合并到支持它们的上游</p>
          <el-table :data="globalApplyPreview" size="small" max-height="300" stripe empty-text="无可应用的上游">
            <el-table-column prop="upstream_name" label="上游账号" min-width="160" show-overflow-tooltip />
            <el-table-column label="将新增的模型" min-width="260">
              <template #default="{ row }">
                <el-tag
                  v-for="model in row.new_models"
                  :key="model"
                  size="small"
                  type="success"
                  class="global-new-model-tag"
                >{{ model }}</el-tag>
              </template>
            </el-table-column>
          </el-table>
        </div>
      </template>

      <el-empty v-else description="没有可用的上游账号（需要 Base URL 和 API Key）" />

      <template #footer>
        <el-button :disabled="globalFetching" @click="globalFetchVisible = false">关闭</el-button>
        <el-button
          type="primary"
          :loading="globalApplying"
          :disabled="globalFetching || globalSelectedModels.length === 0"
          @click="applyGlobalModels"
        >
          应用到支持的上游
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, computed, watch } from 'vue'
import { Activity, CircleCheck, CircleSlash, PlugZap, Plus, RefreshCw, Search, Settings2, Trash2 } from '@lucide/vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import {
  adminApi,
  buildSelectedKeyModelMappings,
  formatModelDiscoveryFailure,
  mergeDiscoveredModelCandidates,
  type BatchCreateUpstreamPayload
} from '@/api/admin'
import type { ApiKeyModelConfig, KeyModelDiscoveryResult, NonstandardFieldPolicy, UpstreamConfig } from '@/types'
import { useTableColumnPreferences, type TableColumnDefinition } from '@/composables/useTableColumns'

const loading = ref(false)
const upstreams = ref<UpstreamConfig[]>([])
const selectedUpstreams = ref<UpstreamConfig[]>([])

const filters = ref({
  status: 'all',
  protocol: 'all',
  credentials: 'all',
  search: ''
})
const upstreamPage = ref(1)
const upstreamPageSize = ref(10)
const tableColumns: TableColumnDefinition[] = [
  { key: 'id', label: 'ID' },
  { key: 'name', label: '名称' },
  { key: 'protocol', label: '协议' },
  { key: 'base_url', label: 'Base URL' },
  { key: 'models', label: '模型数量' },
  { key: 'supported_models', label: '支持的模型' },
  { key: 'keys', label: 'Key 数量' },
  { key: 'key_concurrency', label: '每 Key 最大并发' },
  { key: 'compatibility', label: '兼容清理' },
  { key: 'route_health', label: '路由健康' },
  { key: 'concurrency_gate', label: '并发闸门' },
  { key: 'status', label: '状态' },
  { key: 'priority', label: '优先级/权重' },
  { key: 'remark', label: '备注' }
]
const defaultColumnKeys = tableColumns
  .map(column => column.key)
  .filter(key => key !== 'base_url' && key !== 'supported_models' && key !== 'key_concurrency' && key !== 'route_health')
const { visibleColumnKeys, isColumnVisible } = useTableColumnPreferences(
  tableColumns,
  'admin-upstreams-visible-columns',
  defaultColumnKeys
)

const inlineSaving = ref<Record<string, boolean>>({})
const inlineCommitted = ref<Record<string, { priority: number }>>({})
const dialogVisible = ref(false)
const dialogMode = ref<'create' | 'edit'>('create')
const submitting = ref(false)
const fetchingModels = ref(false)
const discoveredModelCandidates = ref<string[]>([])
const latestDiscoveryResults = ref<KeyModelDiscoveryResult[]>([])
const formRef = ref()
const contextConfigTab = ref<'default' | 'overrides'>('overrides')
const clearDefaultContext = ref(false)

const form = ref<Partial<UpstreamConfig>>({
  id: '',
  name: '',
  remark: '',
  continuation_provider_group: '',
  base_url: '',
  api_key: '',
  protocol: 'ChatCompletions',
  protocols: ['ChatCompletions'],
  api_key_models: [],
  supported_models: [],
  default_model_context: {
    context_limit: 200000,
    output_reserve: 4096,
    max_output_tokens: 0,
    context_group: ''
  },
  active: true,
  model_contexts: [],
  priority: 0,
  strip_nonstandard_chat_fields: 'auto',
  dialect_preset: null,
  model_dialect_presets: {} as Record<string, string>,
  failure_count: 0
})

const modelPresetKeys = ref<string[]>([])

const syncModelPresetKeys = () => {
  modelPresetKeys.value = Object.keys(form.value.model_dialect_presets || {})
}

const addModelPreset = () => {
  if (!form.value.model_dialect_presets) {
    form.value.model_dialect_presets = {}
  }
  const key = `model-${Date.now()}`
  form.value.model_dialect_presets[key] = 'openai'
  syncModelPresetKeys()
}

const removeModelPreset = (pattern: string) => {
  const presets = form.value.model_dialect_presets || {}
  delete presets[pattern]
  syncModelPresetKeys()
}

const renameModelPreset = (oldPattern: string, value: string) => {
  const presets = form.value.model_dialect_presets || {}
  const newPattern = String(value || '').trim()
  if (!newPattern || newPattern === oldPattern) {
    syncModelPresetKeys()
    return
  }
  const preset = presets[oldPattern]
  delete presets[oldPattern]
  presets[newPattern] = preset
  syncModelPresetKeys()
}

const updateModelPreset = (pattern: string, value: string) => {
  const presets = form.value.model_dialect_presets || {}
  presets[pattern] = String(value || '')
}

const availableModelOptions = computed(() => {
  const supported = form.value.supported_models || []
  return Array.from(new Set(supported)).sort()
})

const selectableModelOptions = computed(() => Array.from(new Set([
  ...(form.value.supported_models || []),
  ...discoveredModelCandidates.value
])).sort())

const resetDiscoveryCandidates = () => {
  discoveredModelCandidates.value = []
  latestDiscoveryResults.value = []
}

const addModelContext = () => {
  if (!form.value.model_contexts) {
    form.value.model_contexts = []
  }
  form.value.model_contexts.push({
    slug: '',
    context_limit: 200000,
    output_reserve: 4096,
    max_output_tokens: 0,
    context_group: ''
  })
}

const clearDefaultContextConfig = () => {
  if (!form.value.default_model_context) {
    form.value.default_model_context = {
      context_limit: 0,
      output_reserve: 0,
      max_output_tokens: 0,
      context_group: ''
    }
  } else {
    form.value.default_model_context.context_limit = 0
    form.value.default_model_context.output_reserve = 0
    form.value.default_model_context.max_output_tokens = 0
    form.value.default_model_context.context_group = ''
  }
  clearDefaultContext.value = true
}

const removeModelContext = (row: any) => {
  const index = form.value.model_contexts?.indexOf(row)
  if (index !== undefined && index > -1) {
    form.value.model_contexts?.splice(index, 1)
  }
}

const rules = {
  name: [{ required: true, message: '请输入名称', trigger: 'blur' }],
  base_url: [{ required: true, message: '请输入Base URL', trigger: 'blur' }],
  api_key: [{ required: true, message: '请输入API Key', trigger: 'blur' }],
  max_concurrency: [{ required: true, message: '请输入每 Key 最大并发', trigger: 'change' }],
  protocols: [{ required: true, message: '请选择协议', trigger: 'change' }]
}

const loadData = async () => {
  try {
    loading.value = true
    const { data } = await adminApi.getUpstreams()
    upstreams.value = data
    inlineCommitted.value = Object.fromEntries(
      data.map(row => [row.id, {
        priority: Number(row.priority || 0)
      }])
    )
  } catch (error) {
    ElMessage.error('加载数据失败')
  } finally {
    loading.value = false
  }
}

const inlineSaveKey = (id: string, field: 'priority') => `${id}:${field}`

const isInlineSaving = (id: string, field: 'priority') => {
  return Boolean(inlineSaving.value[inlineSaveKey(id, field)])
}

const updateInlinePriority = async (row: UpstreamConfig) => {
  const field = 'priority' as const
  const saveKey = inlineSaveKey(row.id, field)
  if (inlineSaving.value[saveKey]) return

  const previous = inlineCommitted.value[row.id]?.priority ?? 0
  const priority = Math.max(0, Math.min(1000, Number(row.priority || 0)))
  row.priority = priority
  inlineSaving.value[saveKey] = true
  try {
    const { data } = await adminApi.updateUpstream(row.id, { priority })
    row.priority = Number(data.priority || 0)
    inlineCommitted.value[row.id] = {
      ...(inlineCommitted.value[row.id] || { priority: 0 }),
      priority: row.priority
    }
    ElMessage.success('优先级已更新')
  } catch {
    row.priority = previous
    ElMessage.error('优先级更新失败')
  } finally {
    delete inlineSaving.value[saveKey]
  }
}

const resolveProtocols = (value: Partial<UpstreamConfig>): UpstreamConfig['protocol'][] => {
  const fromProtocols = Array.isArray(value.protocols)
    ? value.protocols.filter(Boolean) as UpstreamConfig['protocol'][]
    : []
  const fallback: UpstreamConfig['protocol'][] = value.protocol
    ? [value.protocol]
    : ['ChatCompletions']
  return Array.from(new Set((fromProtocols.length > 0 ? fromProtocols : fallback)))
}

const displayProtocols = (value: UpstreamConfig) => resolveProtocols(value)

const formatModelList = (models: string[]) => models.length > 0 ? models.join(', ') : '-'

const failureClassLabels: Record<string, string> = {
  credentials: '凭证失败',
  rate_limited: '限流',
  key_quota: 'Key 配额',
  capacity_unavailable: '容量不足',
  transient_server: '临时故障',
  transport: '网络失败',
  concurrency_saturated: '并发饱和'
}

const formatRouteFailureClasses = (health?: UpstreamConfig['route_health']) => {
  const entries = Object.entries(health?.failure_classes ?? {})
    .filter(([key]) => key in failureClassLabels)
  if (entries.length === 0) return ''
  return entries.map(([key, count]) => `${failureClassLabels[key]} ${count}`).join('，')
}

const formatHoldMs = (ms: number) =>
  ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`

const formatRuntimeStateDetail = (row: UpstreamConfig) => {
  const rt = row.runtime_state
  if (!rt) return ''
  const parts: string[] = [`在途 ${rt.in_flight}`]
  if (rt.queue_depth > 0) parts.push(`排队 ${rt.queue_depth}`)
  // G6: 后端明确声明无法观测时，显式渲染「不支持」而不是静默隐藏，
  // 否则运维会把“看不到”误读成“没发生/没有排队”。
  if (rt.queue_depth_supported === false) parts.push('排队深度：本后端不支持')
  if (rt.stale_lease_count > 0) parts.push(`陈旧租约 ${rt.stale_lease_count}`)
  if (rt.oldest_lease_age_seconds > 0) parts.push(`最旧租约 ${rt.oldest_lease_age_seconds}s`)
  if (rt.leaked_reclaimed_total > 0) parts.push(`累计泄漏回收 ${rt.leaked_reclaimed_total}`)
  if (rt.stale_reclaimed_total > 0) parts.push(`累计陈旧回收 ${rt.stale_reclaimed_total}`)
  if (rt.hold_p50_ms != null && rt.hold_p50_ms > 0)
    parts.push(`租约持有 p50 ${formatHoldMs(rt.hold_p50_ms)}`)
  if (rt.hold_p95_ms != null && rt.hold_p95_ms > 0)
    parts.push(`p95 ${formatHoldMs(rt.hold_p95_ms)}`)
  if (rt.hold_supported === false) parts.push('租约持有 p50/p95：本后端不支持')
  if ((rt.capacity_reject_total ?? 0) > 0)
    parts.push(`本地闸门拒绝 ${rt.capacity_reject_total}`)
  if (rt.route_cooldown_skipped_total != null && rt.route_cooldown_skipped_total > 0)
    parts.push(`免冷却放行 ${rt.route_cooldown_skipped_total}`)
  if (rt.route_cooldown_skipped_total == null) parts.push('免冷却放行：本后端不支持')
  if ((rt.sse_bad_frame_skipped_total ?? 0) > 0)
    parts.push(`坏帧跳过 ${rt.sse_bad_frame_skipped_total}`)
  if ((rt.sse_parse_error_total ?? 0) > 0)
    parts.push(`SSE 解析失败 ${rt.sse_parse_error_total}`)
  if ((rt.transport_decode_error_total ?? 0) > 0)
    parts.push(`传输解码失败 ${rt.transport_decode_error_total}`)
  return parts.join('，')
}

const formatRouteCooldown = (health?: UpstreamConfig['route_health']) => {
  const seconds = health?.earliest_retry_after_seconds
  if (!seconds || seconds <= 0) return ''
  if (seconds < 60) return `${seconds} 秒后恢复`
  if (seconds < 3600) return `${Math.ceil(seconds / 60)} 分钟后恢复`
  return `${(seconds / 3600).toFixed(1)} 小时后恢复`
}

const availableProtocols = computed(() => {
  const set = new Set<string>()
  upstreams.value.forEach(item => displayProtocols(item).forEach(p => set.add(p)))
  return Array.from(set).sort()
})

const hasCredentialFailure = (row: UpstreamConfig) =>
  (row.route_health?.failure_classes?.credentials ?? 0) > 0

const filteredUpstreams = computed(() => {
  const keyword = filters.value.search.trim().toLowerCase()
  return upstreams.value.filter(item => {
    if (filters.value.status === 'active' && !item.active) return false
    if (filters.value.status === 'inactive' && item.active) return false
    if (filters.value.protocol !== 'all') {
      const matched = displayProtocols(item).some(p => p === filters.value.protocol)
      if (!matched) return false
    }
    if (filters.value.credentials === 'failing' && !hasCredentialFailure(item)) {
      return false
    }
    if (keyword) {
      const haystack = [item.id, item.name, item.base_url, item.remark]
        .filter(Boolean)
        .join(' ')
        .toLowerCase()
      if (!haystack.includes(keyword)) return false
    }
    return true
  })
})

/** 客户端切片分页：上游数据一次性全量拉取，表格只渲染当前页 */
const pagedUpstreams = computed(() => {
  const start = (upstreamPage.value - 1) * upstreamPageSize.value
  return filteredUpstreams.value.slice(start, start + upstreamPageSize.value)
})

// 筛选条件变化时回到第一页
watch(
  () => [filters.value.status, filters.value.protocol, filters.value.credentials, filters.value.search],
  () => {
    upstreamPage.value = 1
  }
)

// 数据变化导致行数减少时，页码自动回落到有效范围
watch(
  () => filteredUpstreams.value.length,
  () => {
    const maxPage = Math.max(1, Math.ceil(filteredUpstreams.value.length / upstreamPageSize.value))
    if (upstreamPage.value > maxPage) upstreamPage.value = maxPage
  }
)

const isOfficialOpenAIBaseUrl = (baseUrl?: string) => {
  const value = String(baseUrl || '').trim().toLowerCase()
  return value.includes('://api.openai.com') || value.includes('.openai.azure.com')
}

const isAutoChatCompatibility = (value: UpstreamConfig) => {
  return displayProtocols(value).includes('ChatCompletions') && !isOfficialOpenAIBaseUrl(value.base_url)
}

// 旧版本该字段为 boolean（false=auto, true=always_strip），新版本为三态字符串
const normalizeNonstandardPolicy = (value: unknown): NonstandardFieldPolicy => {
  if (value === true) return 'always_strip'
  if (value === false || value === undefined || value === null) return 'auto'
  if (value === 'auto' || value === 'always_strip' || value === 'forward') return value
  return 'auto'
}

const displayKeyCount = (value: UpstreamConfig) => {
  const keys = [
    value.api_key,
    ...(value.api_keys || [])
  ]
    .map(key => String(key || '').trim())
    .filter(Boolean)

  return new Set(keys).size
}

const handleCreate = async () => {
  let defaultMaxConcurrency: number
  try {
    const { data } = await adminApi.getRuntimeSettings()
    defaultMaxConcurrency = Number(data.settings.default_upstream_max_concurrency)
    if (!Number.isSafeInteger(defaultMaxConcurrency) || defaultMaxConcurrency < 1) {
      throw new Error('invalid default_upstream_max_concurrency')
    }
  } catch {
    ElMessage.error('加载新建上游默认并发失败')
    return
  }
  dialogMode.value = 'create'
  contextConfigTab.value = 'overrides'
  clearDefaultContext.value = false
  resetDiscoveryCandidates()
  form.value = {
    id: '',
    name: '',
    remark: '',
    continuation_provider_group: '',
    base_url: '',
    api_key: '',
    protocol: 'ChatCompletions',
    protocols: ['ChatCompletions'],
    api_key_models: [],
    supported_models: [],
    max_concurrency: defaultMaxConcurrency,
    default_model_context: {
      context_limit: 200000,
      output_reserve: 4096,
      max_output_tokens: 0,
      context_group: ''
    },
    active: true,
    model_contexts: [],
    priority: 0,
    strip_nonstandard_chat_fields: 'auto',
    dialect_preset: null,
    model_dialect_presets: {},
    failure_count: 0
  }
  syncModelPresetKeys()
  dialogVisible.value = true
}

const handleCopy = (row: UpstreamConfig) => {
  dialogMode.value = 'create'
  contextConfigTab.value = 'overrides'
  clearDefaultContext.value = false
  resetDiscoveryCandidates()
  const protocols = resolveProtocols(row)
  form.value = {
    id: '',
    name: row.name + ' (副本)',
    remark: row.remark || '',
    continuation_provider_group: row.continuation_provider_group || '',
    base_url: row.base_url,
    api_key: '',
    protocol: protocols[0] as UpstreamConfig['protocol'],
    protocols,
    api_key_models: [],
    supported_models: [...(row.supported_models || [])],
    default_model_context: row.default_model_context
      ? { ...row.default_model_context }
      : { context_limit: 200000, output_reserve: 4096, max_output_tokens: 0, context_group: '' },
    active: row.active,
    model_contexts: row.model_contexts ? [...row.model_contexts] : [],
    priority: row.priority,
    max_concurrency: row.max_concurrency,
    strip_nonstandard_chat_fields: normalizeNonstandardPolicy(row.strip_nonstandard_chat_fields),
    dialect_preset: row.dialect_preset || null,
    model_dialect_presets: row.model_dialect_presets ? { ...row.model_dialect_presets } : {},
    failure_count: 0
  }
  syncModelPresetKeys()
  dialogVisible.value = true
}

const handleEdit = (row: UpstreamConfig) => {
  dialogMode.value = 'edit'
  contextConfigTab.value = 'default'
  clearDefaultContext.value = false
  resetDiscoveryCandidates()
  const protocols = resolveProtocols(row)
  const allKeys = [
    row.api_key,
    ...(row.api_keys || [])
  ]
    .map(key => String(key || '').trim())
    .filter((v, i, a) => a.indexOf(v) === i)
  form.value = {
    ...row,
    continuation_provider_group: row.continuation_provider_group || '',
    api_key: allKeys.join('\n'),
    api_keys: [...(row.api_keys || [])],
    api_key_models: (row.api_key_models || []).map((item: ApiKeyModelConfig) => ({
      api_key: item.api_key,
      supported_models: [...item.supported_models]
    })),
    protocol: protocols[0] as UpstreamConfig['protocol'],
    protocols,
    max_concurrency: row.max_concurrency,
    strip_nonstandard_chat_fields: normalizeNonstandardPolicy(row.strip_nonstandard_chat_fields),
    dialect_preset: row.dialect_preset || null,
    model_dialect_presets: row.model_dialect_presets ? { ...row.model_dialect_presets } : {},
    default_model_context: row.default_model_context
      ? {
          ...row.default_model_context
        }
      : {
          context_limit: 200000,
          output_reserve: 4096,
          max_output_tokens: 0,
          context_group: ''
        },
    model_contexts: row.model_contexts ? [...row.model_contexts] : []
  }
  syncModelPresetKeys()
  dialogVisible.value = true
}

const normalizeModelDialectPresets = (presets: Record<string, string> | undefined): Record<string, string> => {
  const out: Record<string, string> = {}
  const raw = presets || {}
  const keys = Object.keys(raw)
  for (let i = 0; i < keys.length; i++) {
    const pattern = keys[i]
    const editedKey = modelPresetKeys.value[i]
    const p = String(editedKey !== undefined ? editedKey : pattern).trim()
    const v = String(raw[pattern] || '').trim()
    if (p && v) {
      out[p] = v
    }
  }
  return out
}

const handleSubmit = async () => {
  try {
    await formRef.value.validate()
    submitting.value = true

    const submitData: Partial<UpstreamConfig> = {
      ...form.value
    }
    submitData.remark = String(form.value.remark || '').trim()
    submitData.continuation_provider_group =
      String(form.value.continuation_provider_group || '').trim() || null
    delete submitData.requests_per_minute
    delete submitData.request_quota_window_hours
    delete submitData.request_quota_requests
    submitData.max_concurrency = Number(form.value.max_concurrency)
    submitData.model_contexts = (submitData.model_contexts || [])
      .map((item: any) => ({
        slug: String(item.slug || '').trim(),
        context_limit: Number(item.context_limit || 0),
        output_reserve: Number(item.output_reserve || 0),
        max_output_tokens: Number(item.max_output_tokens || 0),
        context_group: String(item.context_group || '').trim()
      }))
      .filter(item => item.slug.length > 0 && item.context_limit > 0)
    if (submitData.default_model_context) {
      const context = submitData.default_model_context
      const context_limit = Number(context.context_limit || 0)
      const output_reserve = Number(context.output_reserve || 0)
      const max_output_tokens = Number(context.max_output_tokens || 0)
      const context_group = String(context.context_group || '').trim()
      if (context_limit > 0) {
        submitData.default_model_context = {
          context_limit,
          output_reserve,
          max_output_tokens,
          context_group
        }
      } else {
        submitData.default_model_context = {
          context_limit: 0,
          output_reserve: 0,
          max_output_tokens: 0,
          context_group: ''
        }
        if (!clearDefaultContext.value) {
          delete submitData.default_model_context
        }
      }
    }
    const protocols = resolveProtocols(submitData)
    submitData.protocols = protocols
    submitData.protocol = protocols[0] as UpstreamConfig['protocol']
    submitData.strip_nonstandard_chat_fields = normalizeNonstandardPolicy(submitData.strip_nonstandard_chat_fields)
    submitData.dialect_preset = form.value.dialect_preset || null
    submitData.model_dialect_presets = normalizeModelDialectPresets(form.value.model_dialect_presets)

    const submittedKeys = (form.value.api_key || '')
      .split('\n')
      .map((key: string) => key.trim())
      .filter((key, index, keys) => key.length > 0 && keys.indexOf(key) === index)

    if (submittedKeys.length === 0) {
      ElMessage.error('请输入至少一个 API Key')
      submitting.value = false
      return
    }

    const selectedModels = Array.from(new Set(
      (form.value.supported_models || [])
        .map((model: any) => String(model || '').trim())
        .filter(Boolean)
    ))
    submitData.api_key = submittedKeys[0]
    submitData.api_keys = submittedKeys.slice(1)
    submitData.supported_models = selectedModels
    submitData.api_key_models = buildSelectedKeyModelMappings(
      submittedKeys,
      selectedModels,
      form.value.api_key_models || [],
      latestDiscoveryResults.value
    )

    if (dialogMode.value === 'create') {
      submitData.id = ''

      if (submittedKeys.length === 1) {
        // 单 key：保持原有行为
        submitData.api_key = submittedKeys[0]
        await adminApi.createUpstream(submitData)
        ElMessage.success('创建成功')
      } else {
        // 多 key：使用 batch 接口提交显式模型映射
        const batchPayload: BatchCreateUpstreamPayload = {
          name: form.value.name!,
          remark: String(form.value.remark || '').trim(),
          continuation_provider_group: submitData.continuation_provider_group,
          base_url: form.value.base_url!,
          keys: submittedKeys,
          supported_models: submitData.supported_models || [],
          api_key_models: submitData.api_key_models || [],
          protocol: protocols[0] ? String(protocols[0]) : 'ChatCompletions',
          protocols: protocols.map(p => String(p)),
          max_concurrency: Number(form.value.max_concurrency),
          active: submitData.active,
          strip_nonstandard_chat_fields: normalizeNonstandardPolicy(submitData.strip_nonstandard_chat_fields),
          dialect_preset: form.value.dialect_preset || null,
          model_dialect_presets: normalizeModelDialectPresets(form.value.model_dialect_presets)
        }

        const response = await adminApi.createUpstreamsBatch(batchPayload)
        const result = response.data

        const keysCount = result.keys_count || 0
        const failedKeys = result.failed || 0

        if (failedKeys > 0 && keysCount > 0) {
          ElMessage.success(`保存了 ${keysCount} 个 Key，${failedKeys} 个 Key 的模型映射暂为空`)
        } else if (keysCount > 0) {
          ElMessage.success(`保存了 ${keysCount} 个 Key`)
        } else {
          const errors = result.results.filter(r => r.error).map(r => r.error).join("；")
          ElMessage.error(`所有 Key 均无效：${errors || "无法验证"}`)
        }
      }
    } else {
      if (submittedKeys.length > 0) {
        // 添加替换标志，让后端替换所有 key 而不是合并
        submitData._replace_api_keys = true
      } else {
        // 用户删除了所有 key，需要清空
        submitData.api_key = ''
        submitData.api_keys = []
        submitData.api_key_models = []
        submitData._replace_api_keys = true
      }
      await adminApi.updateUpstream(form.value.id!, submitData)
      const editTotalKeys = [submitData.api_key, ...(submitData.api_keys || [])].filter(Boolean).length
      if (editTotalKeys > 1) {
        ElMessage.success('更新成功，保存了 ' + editTotalKeys + ' 个 Key')
      } else {
        ElMessage.success('更新成功')
      }
    }
    
    dialogVisible.value = false
    loadData()
  } catch (error: any) {
    if (error.response?.status === 409) {
      ElMessage.error('创建冲突，请重试')
    } else {
      ElMessage.error('操作失败')
    }
  } finally {
    clearDefaultContext.value = false
    submitting.value = false
  }
}

const handleToggle = async (row: UpstreamConfig) => {
  try {
    await adminApi.toggleUpstream(row.id)
    ElMessage.success('状态已更新')
    loadData()
  } catch (error) {
    ElMessage.error('操作失败')
  }
}

const handleResetRouteHealth = async (row: UpstreamConfig) => {
  try {
    await ElMessageBox.confirm(
      `确定解除上游 "${row.name}" 的临时路由冷却吗？Key 凭证和额度状态会保留。`,
      '确认解除临时冷却',
      { type: 'warning' }
    )
    const { data } = await adminApi.resetUpstreamRouteHealth(row.id)
    ElMessage.success(`已解除 ${data.cleared_routes} 条路由的临时冷却`)
    await loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('解除临时冷却失败')
    }
  }
}

const handleResetConcurrency = async (row: UpstreamConfig) => {
  try {
    await ElMessageBox.confirm(
      `确定重置上游 "${row.name}" 的并发闸门吗？将立即释放其全部在途/陈旧租约（可选按 Key 过滤），随后该账号的排队请求会在下一个轮询周期放行。此操作不会中断真正在途的上游请求。`,
      '重置并发闸门',
      { type: 'warning' }
    )
    const { data } = await adminApi.resetUpstreamConcurrency(row.id)
    ElMessage.success(`已释放 ${data.cleared_leases} 个租约`)
    await loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('重置并发闸门失败')
    }
  }
}

const handleDelete = async (row: UpstreamConfig) => {
  try {
    await ElMessageBox.confirm(`确定要删除上游 "${row.name}" 吗？`, '确认删除', {
      type: 'warning'
    })

    await adminApi.deleteUpstream(row.id)
    ElMessage.success('删除成功')
    loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('删除失败')
    }
  }
}

const handleSelectionChange = (rows: UpstreamConfig[]) => {
  selectedUpstreams.value = rows
}

const handleBatchToggle = async (active: boolean) => {
  const ids = selectedUpstreams.value.map(row => row.id)
  if (ids.length === 0) return
  const action = active ? '启用' : '禁用'
  try {
    await ElMessageBox.confirm(`确定要批量${action}选中的 ${ids.length} 个上游吗？`, `批量${action}`, {
      type: 'warning'
    })
    const { data } = await adminApi.batchToggleUpstreams(ids, active)
    const failedCount = data.failed.length
    if (data.updated > 0) {
      ElMessage.success(`已${action} ${data.updated} 个上游${failedCount ? `，${failedCount} 个失败` : ''}`)
    } else {
      ElMessage.error(`批量${action}失败：${data.failed.map(f => f.error).join('；') || '未知错误'}`)
    }
    loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error(`批量${action}失败`)
    }
  }
}

const handleBatchDelete = async () => {
  const ids = selectedUpstreams.value.map(row => row.id)
  if (ids.length === 0) return
  try {
    await ElMessageBox.confirm(
      `确定要删除选中的 ${ids.length} 个上游吗？该操作不可恢复。`,
      '批量删除',
      { type: 'warning' }
    )
    const { data } = await adminApi.batchDeleteUpstreams(ids)
    const failedCount = data.failed.length
    if (data.deleted > 0) {
      ElMessage.success(`已删除 ${data.deleted} 个上游${failedCount ? `，${failedCount} 个失败` : ''}`)
    } else {
      ElMessage.error(`批量删除失败：${data.failed.map(f => f.error).join('；') || '未知错误'}`)
    }
    loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('批量删除失败')
    }
  }
}

interface BatchUpdateForm {
  max_concurrency?: number
  priority?: number
  active: 'keep' | 'true' | 'false'
}

const batchUpdateVisible = ref(false)
const batchUpdating = ref(false)
const batchUpdateForm = ref<BatchUpdateForm>({ active: 'keep' })

const openBatchUpdate = () => {
  batchUpdateForm.value = { active: 'keep' }
  batchUpdateVisible.value = true
}

const resetBatchUpdateForm = () => {
  batchUpdateForm.value = { active: 'keep' }
}

const submitBatchUpdate = async () => {
  const ids = selectedUpstreams.value.map(row => row.id)
  if (ids.length === 0) return
  const updates: Record<string, unknown> = {}
  if (batchUpdateForm.value.max_concurrency !== undefined) {
    updates.max_concurrency = batchUpdateForm.value.max_concurrency
  }
  if (batchUpdateForm.value.priority !== undefined) {
    updates.priority = batchUpdateForm.value.priority
  }
  if (batchUpdateForm.value.active !== 'keep') {
    updates.active = batchUpdateForm.value.active === 'true'
  }
  if (Object.keys(updates).length === 0) {
    ElMessage.warning('请至少填写一个要修改的字段')
    return
  }
  try {
    await ElMessageBox.confirm(
      `确定对选中的 ${ids.length} 个上游应用这些字段修改吗？`,
      '批量修改字段',
      { type: 'warning' }
    )
    batchUpdating.value = true
    const { data } = await adminApi.batchUpdateUpstreams(ids, updates)
    const failedCount = data.failed.length
    if (data.updated.length > 0) {
      ElMessage.success(
        `已更新 ${data.updated.length} 个上游${failedCount ? `，${failedCount} 个失败` : ''}`
      )
    } else {
      ElMessage.error(`批量修改失败：${data.failed.map(f => f.error).join('；') || '未知错误'}`)
    }
    if (failedCount > 0) {
      ElMessage.warning(
        `失败项：${data.failed.map(f => `${f.id}（${f.error}）`).join('；')}`
      )
    }
    batchUpdateVisible.value = false
    loadData()
  } catch (error: any) {
    if (error !== 'cancel') {
      ElMessage.error('批量修改字段失败')
    }
  } finally {
    batchUpdating.value = false
  }
}

const fetchModels = async () => {
  if (!form.value.base_url || !form.value.api_key) {
    ElMessage.warning('请先填写 Base URL 和 API Key')
    return
  }

  // 取所有有效 Key
  const apiKeys = (form.value.api_key || '')
    .split('\n')
    .map(k => k.trim())
    .filter(k => k.length > 0)

  if (apiKeys.length === 0) {
    ElMessage.warning('请输入至少一个有效的 API Key')
    return
  }

  // 取 base_url 第一行（多行粘贴时取第一行有效 URL）
  const baseUrl = (form.value.base_url || '')
    .split('\n')
    .map(u => u.trim())
    .filter(u => u.length > 0)[0] || form.value.base_url

  try {
    fetchingModels.value = true
    const response = await adminApi.discoverUpstreamModels({
      base_url: baseUrl,
      keys: apiKeys
    })
    const result = response.data
    latestDiscoveryResults.value = result.results || []
    discoveredModelCandidates.value = mergeDiscoveredModelCandidates(
      form.value.supported_models || [],
      discoveredModelCandidates.value,
      latestDiscoveryResults.value
    )

    if (!result.models || result.models.length === 0) {
      ElMessage.error(formatModelDiscoveryFailure(result))
      return
    }

    const successCount = (result.total || 0) - (result.failed || 0)
    const parts: string[] = ['成功获取 ' + result.models.length + ' 个模型']
    if (successCount > 1) {
      parts.push('用了 ' + successCount + ' 个 Key')
    }
    if (result.failed > 0) {
      const failedKeys = (result.results || [])
        .filter(item => item.error)
        .map(item => `Key #${item.key_index + 1}`)
        .join('、')
      parts.push(result.failed + ' 个 Key 获取失败' + (failedKeys ? `（${failedKeys}）` : ''))
    }
    ElMessage.success(parts.join('，'))
  } catch (error: any) {
    ElMessage.error('获取模型失败: ' + error.message)
  } finally {
    fetchingModels.value = false
  }
}

// ---- 全局获取模型：一键拉取所有上游账号的模型，勾选后自动应用到支持的上游 ----
const globalFetchVisible = ref(false)
const globalFetching = ref(false)
const globalApplying = ref(false)
const globalProgress = ref({ done: 0, total: 0 })
const globalSelectedModels = ref<string[]>([])
/** upstream id -> { name, models } */
const globalUpstreamModels = ref<Record<string, { name: string; models: string[] }>>({})

/** 全部上游发现的模型去重排序 */
const globalModelPool = computed(() => {
  const set = new Set<string>()
  for (const entry of Object.values(globalUpstreamModels.value)) {
    for (const model of entry.models) set.add(model)
  }
  return Array.from(set).sort()
})

/** 某个模型被几个上游账号支持 */
const globalModelAccountCount = (model: string) =>
  Object.values(globalUpstreamModels.value).filter(entry => entry.models.includes(model)).length

/** 应用预览：每个上游将新增的模型（选中 ∩ 该上游支持 - 已有） */
const globalApplyPreview = computed(() => {
  const selected = new Set(globalSelectedModels.value)
  const preview: Array<{ upstream_name: string; new_models: string[] }> = []
  for (const [id, entry] of Object.entries(globalUpstreamModels.value)) {
    const existing = new Set((upstreams.value.find(u => u.id === id)?.supported_models) || [])
    const newModels = entry.models
      .filter(model => selected.has(model) && !existing.has(model))
      .sort()
    if (newModels.length > 0) {
      preview.push({ upstream_name: entry.name, new_models: newModels })
    }
  }
  return preview
})

const openGlobalFetch = async () => {
  globalFetchVisible.value = true
  globalFetching.value = true
  globalSelectedModels.value = []
  globalUpstreamModels.value = {}

  const targets = upstreams.value.filter(
    upstream =>
      upstream.base_url &&
      ((upstream.api_key || '').trim() || (upstream.api_keys || []).some(key => key.trim()))
  )

  if (targets.length === 0) {
    globalFetching.value = false
    ElMessage.warning('没有可用的上游账号（需要配置 Base URL 和 API Key）')
    return
  }

  globalProgress.value = { done: 0, total: targets.length }
  let succeeded = 0
  let failed = 0

  for (const upstream of targets) {
    const apiKeys = [
      upstream.api_key || '',
      ...(upstream.api_keys || [])
    ]
      .map(key => key.trim())
      .filter(key => key.length > 0)
    const uniqueKeys = Array.from(new Set(apiKeys))

    try {
      const response = await adminApi.discoverUpstreamModels({
        base_url: upstream.base_url,
        keys: uniqueKeys
      })
      const models = (response.data.models || []).filter(Boolean)
      if (models.length > 0) {
        globalUpstreamModels.value[upstream.id] = {
          name: upstream.name || upstream.id,
          models: Array.from(new Set(models)).sort()
        }
        succeeded++
      } else {
        failed++
      }
    } catch (error: any) {
      failed++
      console.error(`获取上游 ${upstream.name || upstream.id} 模型失败:`, error?.message || error)
    }
    globalProgress.value.done++
  }

  globalFetching.value = false
  if (succeeded === 0) {
    ElMessage.error(`获取失败：${failed} 个上游账号均未能获取到模型`)
  } else if (failed > 0) {
    ElMessage.warning(`成功获取 ${succeeded} 个上游账号的模型，${failed} 个账号获取失败`)
  } else {
    ElMessage.success(`成功获取 ${succeeded} 个上游账号的模型`)
  }
}

const applyGlobalModels = async () => {
  const selected = new Set(globalSelectedModels.value)
  if (selected.size === 0) {
    ElMessage.warning('请先选择要全局启用的模型')
    return
  }

  globalApplying.value = true
  const appliedIds: string[] = []
  const failedIds: string[] = []
  let totalAdded = 0

  try {
    for (const [id, entry] of Object.entries(globalUpstreamModels.value)) {
      const upstream = upstreams.value.find(u => u.id === id)
      if (!upstream) continue
      const existing = new Set(upstream.supported_models || [])
      const additions = entry.models.filter(model => selected.has(model) && !existing.has(model))
      if (additions.length === 0) continue

      const merged = Array.from(new Set([...(upstream.supported_models || []), ...additions])).sort()
      try {
        // 只更新 supported_models，不携带 _replace_api_keys，不会改动 API Key
        await adminApi.updateUpstream(id, { supported_models: merged })
        appliedIds.push(entry.name)
        totalAdded += additions.length
      } catch (error: any) {
        failedIds.push(`${entry.name}（${error?.message || '未知错误'}）`)
      }
    }

    if (appliedIds.length > 0) {
      ElMessage.success(`已为 ${appliedIds.length} 个上游账号自动选中 ${totalAdded} 个模型`)
    }
    if (failedIds.length > 0) {
      ElMessage.error(`更新失败：${failedIds.join('；')}`)
    }
    if (appliedIds.length === 0 && failedIds.length === 0) {
      ElMessage.info('所选模型已全部存在于各上游账号中，无需更新')
    }

    globalFetchVisible.value = false
    await loadData()
  } finally {
    globalApplying.value = false
  }
}

const resetGlobalFetch = () => {
  globalSelectedModels.value = []
  globalUpstreamModels.value = {}
  globalProgress.value = { done: 0, total: 0 }
}

// 批量选择功能
const selectAllModels = () => {
  globalSelectedModels.value = [...globalModelPool.value]
}

const invertSelection = () => {
  const selected = new Set(globalSelectedModels.value)
  globalSelectedModels.value = globalModelPool.value.filter(m => !selected.has(m))
}

const clearSelection = () => {
  globalSelectedModels.value = []
}

// 预设模型组
interface ModelGroup {
  id: string
  name: string
  pattern: string
}

const builtinModelGroups: ModelGroup[] = [
  {
    id: 'gpt',
    name: 'GPT 系列',
    pattern: '^gpt-'
  },
  {
    id: 'claude',
    name: 'Claude 系列',
    pattern: '^claude-'
  },
  {
    id: 'opensource',
    name: '开源模型',
    pattern: '^(llama|mistral|mixtral|qwen|deepseek)-'
  }
]

const applyModelGroup = (group: ModelGroup) => {
  const regex = new RegExp(group.pattern, 'i')
  const groupModels = globalModelPool.value.filter(m => regex.test(m))
  const selected = new Set(globalSelectedModels.value)
  groupModels.forEach(m => selected.add(m))
  globalSelectedModels.value = Array.from(selected)
}

// 筛选功能
const globalFilterText = ref('')

const filteredModels = computed(() => {
  let models = globalModelPool.value

  // 文本筛选（支持正则）
  if (globalFilterText.value.trim()) {
    try {
      const regex = new RegExp(globalFilterText.value, 'i')
      models = models.filter(m => regex.test(m))
    } catch {
      // 如果不是有效正则，fallback 到普通字符串匹配
      const search = globalFilterText.value.toLowerCase()
      models = models.filter(m => m.toLowerCase().includes(search))
    }
  }

  return models
})


onMounted(() => {
  loadData()
})
</script>

<style scoped>

.upstream-batch-actions {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}

.upstreams-page {
  min-height: 100%;
}

.protocol-cell {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  width: 100%;
}

.base-url-cell,
.model-list-cell {
  display: inline-block;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: middle;
}

.base-url-cell {
  font-family: var(--crc-font-mono);
}

.model-input-group {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  width: 100%;
}

.model-input-group :deep(.el-select) {
  flex: 1;
}

.fetch-btn {
  white-space: nowrap;
}

.helper-text {
  margin-top: 8px;
}

.global-fetch-progress {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 24px 8px;
  color: var(--el-text-color-secondary);
}

.global-fetch-summary {
  margin-bottom: 12px;
  font-size: 13px;
  color: var(--el-text-color-regular);
  line-height: 1.6;
}

.global-model-form {
  margin-bottom: 4px;
}

.global-apply-preview {
  margin-top: 8px;
}

.global-apply-title {
  margin: 0 0 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--el-text-color-primary);
}

.global-new-model-tag {
  margin: 2px 4px 2px 0;
}

.form-hint {
  display: block;
  width: 100%;
  margin-top: 6px;
  color: var(--crc-text-muted);
  font-size: 12px;
}

:global(.form-drawer .el-drawer__header) {
  margin-bottom: 0;
  padding: 16px 24px;
  border-bottom: 1px solid var(--crc-border);
}

:global(.form-drawer .el-drawer__body) {
  padding: 24px 32px;
  overflow-y: auto;
}

:global(.form-drawer .el-drawer__footer) {
  border-top: 1px solid var(--crc-border);
  padding: 12px 24px;
  background: var(--crc-surface);
}

.drawer-form {
  width: 100%;
}

.drawer-section {
  margin: 28px 0 20px;
}

.drawer-section :deep(.el-divider__text) {
  color: var(--crc-text-strong);
  font-size: 13px;
  font-weight: 600;
}

.drawer-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

@media (max-width: 767px) {
  :global(.form-drawer .el-drawer__body) {
    padding: 18px 16px;
  }

  .model-input-group {
    flex-direction: column;
  }

  .fetch-btn {
    width: 100%;
  }
}

.drawer-section :deep(.el-divider__text) {
  font-family: var(--crc-font-mono);
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

.protocol-cell :deep(.el-tag) {
  font-family: var(--crc-font-mono);
}

.form-hint {
  font-family: var(--crc-font-mono);
  font-size: 11px;
}

.upstream-filters :deep(.el-form-item) {
  margin-right: 18px;
  margin-bottom: 8px;
}

.upstream-filters :deep(.el-form-item__label) {
  font-family: var(--crc-font-mono);
  font-size: 11px;
  letter-spacing: 0.06em;
}

.upstream-filters :deep(.el-select) {
  min-width: 150px;
}

.upstream-filters :deep(.el-input) {
  min-width: 220px;
}

.table-column-settings-item {
  margin-right: 0;
  margin-bottom: 8px;
}

.filter-label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.filter-label :deep(svg) {
  color: var(--crc-accent);
}

.upstream-table-pagination {
  display: flex;
  justify-content: flex-end;
  margin-top: 14px;
}

.batch-update-hint {
  margin: 0 0 12px;
  font-size: 13px;
  color: var(--crc-text-muted, #888);
}

.batch-update-clear {
  margin-left: 10px;
  font-size: 12px;
  color: var(--crc-accent);
  cursor: pointer;
  user-select: none;
}

@media (max-width: 767px) {
  .upstream-filters :deep(.el-select),
  .upstream-filters :deep(.el-input) {
    min-width: 0;
    width: 100%;
  }
}
</style>
