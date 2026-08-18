import { describe, expect, it, vi } from 'vitest'
import { copyTextToClipboard } from '@/utils/clipboard'

describe('copyTextToClipboard', () => {
  it('prefers the clipboard API', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('navigator', { clipboard: { writeText } })

    await expect(copyTextToClipboard('secret')).resolves.toBe(true)
    expect(writeText).toHaveBeenCalledWith('secret')
  })

  it('uses the textarea fallback when clipboard API rejects', async () => {
    const writeText = vi.fn().mockRejectedValue(new Error('denied'))
    const textarea = {
      value: '',
      style: {},
      setAttribute: vi.fn(),
      focus: vi.fn(),
      select: vi.fn()
    }
    const execCommand = vi.fn().mockReturnValue(true)
    vi.stubGlobal('navigator', { clipboard: { writeText } })
    vi.stubGlobal('document', {
      createElement: vi.fn().mockReturnValue(textarea),
      body: { appendChild: vi.fn(), removeChild: vi.fn() },
      execCommand
    })

    await expect(copyTextToClipboard('secret')).resolves.toBe(true)
    expect(execCommand).toHaveBeenCalledWith('copy')
  })
})
