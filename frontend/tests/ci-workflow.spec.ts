import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

describe('CI workflow', () => {
  it('runs frontend and backend quality gates', () => {
    const workflow = readFileSync(
      new URL('../../.github/workflows/ci.yml', import.meta.url),
      'utf8'
    )

    expect(workflow).toContain('npm --prefix frontend ci')
    expect(workflow).toContain('npm --prefix frontend test')
    expect(workflow).toContain('npm --prefix frontend run type-check')
    expect(workflow).toContain('cargo fmt --all --check')
    expect(workflow).toContain('cargo clippy --all-targets -- -D warnings')
    expect(workflow).toContain('cargo test --all')
  })
})
