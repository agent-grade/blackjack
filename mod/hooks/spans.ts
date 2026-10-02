import type { PromptDecoration } from 'claude-code'

export type Flag = { start: number; end: number; kind: 'spelling' | 'grammar' }

const COLOR = { spelling: 'red', grammar: 'blueBright' } as const

export function codePointToUnit(text: string): number[] {
  const units = [0]
  let unit = 0
  for (const character of text) {
    unit += character.length
    units.push(unit)
  }
  return units
}

const WORD_CHARACTER = /^[\p{L}\p{N}]/u

export function isBeingTyped(text: string, cursor: number, start: number, end: number): boolean {
  if (cursor <= start || cursor > end) return false
  return cursor < end || WORD_CHARACTER.test(text.slice(cursor)) || cursor === text.length
}

export function decorationsOf(text: string, flags: Flag[], cursor: number): PromptDecoration[] {
  const units = codePointToUnit(text)
  const unitOf = (codePoint: number) => units[codePoint] ?? text.length
  return flags
    .map(flag => ({ start: unitOf(flag.start), end: unitOf(flag.end), color: COLOR[flag.kind] }))
    .filter(({ start, end }) => !isBeingTyped(text, cursor, start, end))
    .map(({ start, end, color }) => ({ start, end, underline: true, color }))
}
