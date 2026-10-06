import type { PromptDecoration } from 'claude-code'

export type Flag = { start: number; end: number; kind: 'spelling' | 'grammar'; offers: string[] }

const COLOR = { spelling: 'red', grammar: 'blueBright' } as const

const WORD_CHARACTER = /^[\p{L}\p{N}]/u

export function isBeingTyped(text: string, cursor: number, start: number, end: number): boolean {
  if (cursor <= start || cursor > end) return false
  return cursor < end || WORD_CHARACTER.test(text.slice(cursor)) || cursor === text.length
}

export function decorationsOf(text: string, flags: Flag[], cursor: number): PromptDecoration[] {
  return flags
    .filter(({ start, end }) => !isBeingTyped(text, cursor, start, end))
    .map(({ start, end, kind }) => ({ start, end, color: COLOR[kind] }))
}
