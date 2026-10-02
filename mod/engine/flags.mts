import type { Lint, LintConfig, LocalLinter } from './vendor/harper/index.js'
import { filtered, intact } from './prose.mts'

export const SPELLING = 'SpellCheck'

export const GRAMMAR = [
  'AnA',
  'MissingDeterminer',
  'ItsContraction',
  'ItsPossessive',
  'LetsConfusion',
  'RepeatedWords',
  'Spaces',
  'NoFrenchSpaces',
  'ToTwoToo',
  'ThenThan',
]

type Span = { start: number; end: number }
export type Flag = Span & { kind: 'spelling' | 'grammar' }

export function enabledConfig(config: LintConfig): LintConfig {
  return Object.fromEntries(Object.keys(config).map(rule => [rule, rule === SPELLING || GRAMMAR.includes(rule)]))
}

const isWord = (c: string) => /[\p{L}\p{N}]/u.test(c)

function wordAfter(characters: string[], from: number): number {
  let to = from
  for (let at = from; at < characters.length && !/\s/.test(characters[at]!); at++) {
    if (isWord(characters[at]!)) to = at + 1
  }
  return to
}

export function anchored(characters: string[], { start, end }: Span, insertsWord: boolean): Span | undefined {
  const blank = end > start && characters.slice(start, end).every(c => /\s/.test(c))
  if (!blank || !insertsWord) return { start, end }
  const to = wordAfter(characters, end)
  return to > end ? { start: end, end: to } : undefined
}

export function flagsOf(text: string, organized: Record<string, Lint[]>): Flag[] {
  const characters = Array.from(text)
  const flags: Flag[] = []
  for (const [rule, lints] of Object.entries(organized)) {
    const kind = rule === SPELLING ? 'spelling' : 'grammar'
    for (const lint of lints) {
      const span = lint.span()
      const insertsWord = lint.suggestions().some(s => s.get_replacement_text().trim() !== '')
      const at = anchored(characters, span, insertsWord)
      if (at) flags.push({ ...at, kind })
    }
  }
  return flags.sort((a, b) => a.start - b.start || a.end - b.end)
}

export async function lint(linter: LocalLinter, text: string): Promise<Flag[]> {
  const prose = filtered(text)
  return flagsOf(prose, await linter.organizedLints(prose, { language: 'plaintext' })).filter(flag => intact(text, flag))
}
