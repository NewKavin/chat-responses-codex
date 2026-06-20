import { describe, expect, it } from 'vitest'
import {
  buildPlaygroundChatPayload,
  extractChatCompletionText,
  parseGatewayModels,
  type UploadedFileContext
} from './playground'

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
