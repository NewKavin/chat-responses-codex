import { describe, expect, it } from 'vitest'
import { modelMappingStatusPresentation } from './modelMappingStatus'

describe('model mapping status', () => {
  it('presents backend-authoritative mapping states and reasons', () => {
    expect(modelMappingStatusPresentation(
      'effective',
      'eligible_routes_available'
    )).toEqual({
      label: '生效',
      tagType: 'success',
      reasonLabel: '存在符合能力要求的精确路由'
    })
    expect(modelMappingStatusPresentation(
      'partial',
      'some_routes_ineligible'
    )).toEqual({
      label: '部分生效',
      tagType: 'warning',
      reasonLabel: '部分精确路由不符合能力要求'
    })
    expect(modelMappingStatusPresentation(
      'inactive',
      'upstream_inactive'
    )).toEqual({
      label: '未生效',
      tagType: 'danger',
      reasonLabel: '上游未启用'
    })
  })

  it('stays neutral when status data is missing or the backend reason is unknown', () => {
    expect(modelMappingStatusPresentation('effective', 'future_reason')).toEqual({
      label: '状态未知',
      tagType: 'info',
      reasonLabel: 'future_reason'
    })
    expect(modelMappingStatusPresentation(undefined, undefined)).toEqual({
      label: '状态未知',
      tagType: 'info',
      reasonLabel: '未能读取后端映射状态'
    })
  })
})
