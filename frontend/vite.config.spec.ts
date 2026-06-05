import { describe, expect, it } from 'vitest'
import config from './vite.config'

describe('vite proxy config', () => {
  it('proxies API requests to the gateway on port 3001', () => {
    const apiProxy = config.server?.proxy?.['/api'] as { target?: string } | undefined
    const v1Proxy = config.server?.proxy?.['/v1'] as { target?: string } | undefined

    expect(apiProxy?.target).toBe('http://localhost:3001')
    expect(v1Proxy?.target).toBe('http://localhost:3001')
  })
})
