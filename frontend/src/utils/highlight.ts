import hljs from 'highlight.js/lib/core'
import bash from 'highlight.js/lib/languages/bash'
import javascript from 'highlight.js/lib/languages/javascript'
import json from 'highlight.js/lib/languages/json'
import python from 'highlight.js/lib/languages/python'
import typescript from 'highlight.js/lib/languages/typescript'

const languageAliases: Record<string, string> = {
  js: 'javascript',
  jsx: 'javascript',
  sh: 'bash',
  shell: 'bash',
  ts: 'typescript',
  tsx: 'typescript'
}

export const registeredHighlightLanguages = [
  'bash',
  'json',
  'javascript',
  'python',
  'typescript'
] as const

hljs.registerLanguage('bash', bash)
hljs.registerLanguage('json', json)
hljs.registerLanguage('javascript', javascript)
hljs.registerLanguage('python', python)
hljs.registerLanguage('typescript', typescript)

const resolveLanguage = (lang?: string) => {
  const normalized = lang?.trim().toLowerCase()
  if (!normalized) return 'plaintext'

  const language = languageAliases[normalized] ?? normalized
  return registeredHighlightLanguages.includes(language as (typeof registeredHighlightLanguages)[number])
    ? language
    : 'plaintext'
}

const escapeHtml = (value: string) => {
  return value.replace(/[&<>"']/g, character => {
    switch (character) {
      case '&':
        return '&amp;'
      case '<':
        return '&lt;'
      case '>':
        return '&gt;'
      case '"':
        return '&quot;'
      case "'":
        return '&#39;'
      default:
        return character
    }
  })
}

export const createHighlightedCodeRenderer = () => {
  return ({ text, lang }: { text: string; lang?: string }) => {
    const language = resolveLanguage(lang)
    const highlighted =
      language === 'plaintext'
        ? escapeHtml(text)
        : hljs.highlight(text, { language }).value
    return `<pre><code class="hljs language-${language}">${highlighted}</code></pre>`
  }
}
