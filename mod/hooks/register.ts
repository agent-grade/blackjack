import type { PromptDecoration, Register } from 'claude-code'

const FLAGGED = /\bteh\b/g
const FIX_TRIGGER = ';'

function flag(text: string): PromptDecoration[] {
  return [...text.matchAll(FLAGGED)].map(m => ({
    start: m.index,
    end: m.index + m[0].length,
    underline: true,
    color: 'warning',
  }))
}

export const register: Register = on => {
  on('prompt.edit', async ($, e, next) => {
    const isChord =
      e.inputText === FIX_TRIGGER && e.start === e.end && e.text[e.start - 1] === FIX_TRIGGER
    if (isChord) {
      const before = e.text.slice(0, e.start - 1).replace(FLAGGED, 'the')
      const after = e.text.slice(e.start).replace(FLAGGED, 'the')
      const text = before + after
      return { text, cursor: before.length, decorations: flag(text) }
    }
    const r = await next(e)
    return { ...r, decorations: [...(r.decorations ?? []), ...flag(r.text)] }
  })
}
