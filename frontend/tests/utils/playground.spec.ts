import { describe, expect, it } from 'vitest'
import {
  buildPlaygroundChatPayload,
  extractChatCompletionText,
  formatPlaygroundCompletionMeta,
  formatPlaygroundStreamStatus,
  formatPlaygroundUsageText,
  parseGatewayModels,
  parseSSELine,
  type UploadedFileContext
} from '../../src/utils/playground'

describe('playground model parsing', () => {
  it('deduplicates and trims model ids from gateway response', () => {
    const models = parseGatewayModels({
      data: [
        { id: ' gpt-4 ' },
        { id: 'gpt-3.5-turbo' },
        { id: 'gpt-4' },
        { id: '  ' },
        { id: 123 },
        { id: null },
        { notId: 'skip' }
      ]
    })

    expect(models).toEqual(['gpt-4', 'gpt-3.5-turbo'])
  })

  it('returns empty list when data missing', () => {
    expect(parseGatewayModels({})).toEqual([])
  })

  it('throws on non-object model response', () => {
    expect(() => parseGatewayModels('nope')).toThrow('模型列表返回结构不正确')
  })
})

describe('playground chat payload', () => {
  it('builds message list with trimmed content', () => {
    const payload = buildPlaygroundChatPayload({
      model: 'gpt-4',
      question: '  hello  ',
      temperature: 1.234,
      maxTokens: 2048.9,
      history: [
        { role: 'user', content: '  你好  ' },
        { role: 'assistant', content: '你好！' },
        { role: 'system', content: '  System ' }
      ]
    })

    expect(payload.model).toBe('gpt-4')
    expect(payload.stream).toBe(false)
    expect(payload.temperature).toBe(1.23)
    expect(payload.max_tokens).toBe(2048)
    expect(payload.messages).toEqual([
      { role: 'user', content: '你好' },
      { role: 'assistant', content: '你好！' },
      { role: 'system', content: 'System' },
      { role: 'user', content: 'hello' }
    ])
  })

  it('includes attached file content as content blocks', () => {
    const uploadedFiles: UploadedFileContext[] = [
      {
        name: 'note.txt',
        type: 'text/plain',
        size: 12,
        text: '第一段'
      },
      {
        name: 'data.json',
        type: 'application/json',
        size: 9,
        text: '{"k":"v"}'
      }
    ]

    const payload = buildPlaygroundChatPayload({
      model: 'gpt-4',
      question: '请基于文件回答',
      uploadedFiles
    })

    expect(payload.messages).toEqual([
      {
        role: 'user',
        content: [
          {
            type: 'text',
            text: '【文件】note.txt (text/plain, 12B)\n第一段'
          },
          {
            type: 'text',
            text: '【文件】data.json (application/json, 9B)\n{"k":"v"}'
          },
          {
            type: 'text',
            text: '请基于文件回答'
          }
        ]
      }
    ])
  })

  it('falls back to plain text when no files are attached', () => {
    const payload = buildPlaygroundChatPayload({
      model: 'gpt-4',
      question: 'just text',
      uploadedFiles: []
    })

    expect(payload.messages).toEqual([{ role: 'user', content: 'just text' }])
  })

  it('drops invalid history entries and skips empty max tokens', () => {
    const payload = buildPlaygroundChatPayload({
      model: 'gpt-4',
      question: 'test',
      maxTokens: 0,
      temperature: -1,
      history: [
        { role: 'user' as const, content: 'A' },
        { role: 'assistant', content: '' },
        { role: 'assistant', content: '   ' },
        { role: 'system', content: 'ok' }
      ]
    })

    expect(payload).not.toHaveProperty('max_tokens')
    expect(payload).not.toHaveProperty('temperature')
    expect(payload.messages).toEqual([
      { role: 'user', content: 'A' },
      { role: 'assistant', content: '' },
      { role: 'assistant', content: '' },
      { role: 'system', content: 'ok' },
      { role: 'user', content: 'test' }
    ])
  })

  it('includes inference strength when provided', () => {
    const payload = buildPlaygroundChatPayload({
      model: 'gpt-4',
      question: 'test',
      inferenceStrength: 'high' as any
    } as any)

    expect((payload as any).inference_strength).toBe('high')
  })
})

describe('playground response extraction', () => {
  it('reads assistant message content from chat completion response', () => {
    const text = extractChatCompletionText({
      choices: [
        {
          message: {
            content: 'Hello world'
          }
        }
      ]
    })

    expect(text).toBe('Hello world')
  })

  it('returns empty text when message content is not string', () => {
    const text = extractChatCompletionText({
      choices: [
        {
          message: {
            content: null
          }
        }
      ]
    })

    expect(text).toBe('')
  })

  it('throws when response has no choices', () => {
    expect(() => extractChatCompletionText({})).toThrow('响应缺少 choices')
  })
})

describe('formatPlaygroundStreamStatus', () => {
  it('shows a connected waiting state when only keepalives arrived', () => {
    expect(
      formatPlaygroundStreamStatus({
        phase: 'waiting',
        elapsedSeconds: 12,
        keepaliveCount: 3
      })
    ).toBe('已连接，等待模型首个输出 12s')
  })

  it('shows a long waiting state after 30 seconds without first output', () => {
    expect(
      formatPlaygroundStreamStatus({
        phase: 'waiting',
        elapsedSeconds: 31,
        keepaliveCount: 2
      })
    ).toBe('模型仍在处理，已等待首个输出 31s')
  })

  it('keeps the connecting state while the stream has not connected', () => {
    expect(
      formatPlaygroundStreamStatus({
        phase: 'connecting',
        elapsedSeconds: 7,
        keepaliveCount: 2
      })
    ).toBe('正在连接模型 7s')
  })

  it('shows thinking and generating states when output has started', () => {
    expect(
      formatPlaygroundStreamStatus({
        phase: 'thinking',
        elapsedSeconds: 8,
        keepaliveCount: 1
      })
    ).toBe('思考中 8s')
    expect(
      formatPlaygroundStreamStatus({
        phase: 'generating',
        elapsedSeconds: 9,
        keepaliveCount: 1
      })
    ).toBe('生成中 9s')
  })
})

describe('parseSSELine', () => {
  it('marks chat keepalive comments as stream activity', () => {
    const chunk = parseSSELine(': keepalive')
    expect(chunk).not.toBeNull()
    expect(chunk!.keepalive).toBe(true)
    expect(chunk!.content).toBe('')
    expect(chunk!.done).toBe(false)
  })

  it('marks empty data objects as keepalive activity', () => {
    const chunk = parseSSELine('data: {}')
    expect(chunk).not.toBeNull()
    expect(chunk!.keepalive).toBe(true)
    expect(chunk!.content).toBe('')
    expect(chunk!.done).toBe(false)
  })

  it('extracts structured stream error frames', () => {
    const line =
      'data: {"error":{"message":"upstream quota exceeded","type":"upstream_error","code":"quota_exceeded","category":"upstream_rate_limited"}}'
    const chunk = parseSSELine(line)
    expect(chunk).not.toBeNull()
    expect(chunk!.errorMessage).toBe('upstream quota exceeded')
    expect(chunk!.errorType).toBe('upstream_error')
    expect(chunk!.errorCode).toBe('quota_exceeded')
    expect(chunk!.errorCategory).toBe('upstream_rate_limited')
  })

  it('extracts reasoning_content delta from deepseek-style stream chunks', () => {
    const line = 'data: {"choices":[{"index":0,"delta":{"reasoning_content":"思考中","content":""}}]}'
    const chunk = parseSSELine(line)
    expect(chunk).not.toBeNull()
    expect(chunk!.reasoningContent).toBe('思考中')
    expect(chunk!.content).toBe('')
  })

  it('extracts content delta alongside reasoning_content', () => {
    const line = 'data: {"choices":[{"index":0,"delta":{"reasoning_content":"","content":"答案"}}]}'
    const chunk = parseSSELine(line)
    expect(chunk!.content).toBe('答案')
    expect(chunk!.reasoningContent).toBe('')
  })

  it('returns undefined reasoningContent when field is absent', () => {
    const line = 'data: {"choices":[{"index":0,"delta":{"content":"hi"}}]}'
    const chunk = parseSSELine(line)
    expect(chunk!.content).toBe('hi')
    expect(chunk!.reasoningContent).toBeUndefined()
  })
})

describe('playground display helpers', () => {
  it('formats usage and elapsed metadata', () => {
    expect(
      formatPlaygroundUsageText({
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30
      })
    ).toBe('tokens: in=10 out=20 total=30')

    expect(
      formatPlaygroundCompletionMeta({
        model: 'glm-5.1',
        elapsedSeconds: 12,
        firstOutputSeconds: 5,
        usageText: 'tokens: in=10 out=20 total=30'
      })
    ).toBe('模型 glm-5.1 · 总耗时 12s · 首包 5s · tokens: in=10 out=20 total=30')
  })
})
