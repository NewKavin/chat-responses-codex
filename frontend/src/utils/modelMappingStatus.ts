import type { ModelMappingStatus, ModelMappingStatusReason } from '@/types'

export interface ModelMappingStatusPresentation {
  label: string
  tagType: 'success' | 'warning' | 'danger' | 'info'
  reasonLabel: string
}

const STATUS_PRESENTATION: Record<
  ModelMappingStatus,
  Pick<ModelMappingStatusPresentation, 'label' | 'tagType'>
> = {
  effective: { label: '生效', tagType: 'success' },
  partial: { label: '部分生效', tagType: 'warning' },
  inactive: { label: '未生效', tagType: 'danger' }
}

const REASON_LABELS: Record<ModelMappingStatusReason, string> = {
  eligible_routes_available: '存在符合能力要求的精确路由',
  some_routes_ineligible: '部分精确路由不符合能力要求',
  upstream_inactive: '上游未启用',
  upstream_model_unavailable: '上游模型已不可用',
  no_key_for_upstream_model: '该模型没有可用 Key',
  no_eligible_routes: '没有符合能力要求的精确路由'
}

const knownStatus = (status: string): status is ModelMappingStatus =>
  Object.prototype.hasOwnProperty.call(STATUS_PRESENTATION, status)

const knownReason = (reason: string): reason is ModelMappingStatusReason =>
  Object.prototype.hasOwnProperty.call(REASON_LABELS, reason)

export const modelMappingStatusPresentation = (
  status: string | null | undefined,
  reason: string | null | undefined
): ModelMappingStatusPresentation => {
  if (!status || !reason || !knownStatus(status) || !knownReason(reason)) {
    return {
      label: '状态未知',
      tagType: 'info',
      reasonLabel: reason || '未能读取后端映射状态'
    }
  }
  return {
    ...STATUS_PRESENTATION[status],
    reasonLabel: REASON_LABELS[reason]
  }
}
