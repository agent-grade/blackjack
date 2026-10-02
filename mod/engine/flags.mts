import { SuggestionKind, type Lint, type LintConfig, type LocalLinter } from './vendor/harper/index.js'
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
export type Flag = Span & { kind: 'spelling' | 'grammar'; offers: string[] }

export function enabledConfig(config: LintConfig): LintConfig {
  return Object.fromEntries(Object.keys(config).map(rule => [rule, rule === SPELLING || GRAMMAR.includes(rule)]))
}

function offersOf(lint: Lint, word: string): string[] {
  return lint.suggestions().map(s => {
    const text = s.get_replacement_text()
    if (s.kind() === SuggestionKind.Replace) return text
    if (s.kind() === SuggestionKind.InsertAfter) return word + text
    return ''
  })
}

export function anchored(text: string, { start, end }: Span, offers: string[]): Span & { offers: string[] } {
  const blank = end > start && /^\s+$/.test(text.slice(start, end))
  const insertsWord = offers.some(offer => offer.trim() !== '')
  if (!blank || !insertsWord) return { start, end, offers }
  const to = end + (text.slice(end).match(/^\S*[\p{L}\p{N}]/u)?.[0].length ?? 0)
  if (to <= end) return { start: end, end, offers: [] }
  const word = text.slice(end, to)
  return { start: end, end: to, offers: offers.filter(o => o.trim() !== '').map(o => `${o.trim()} ${word}`) }
}

export function flagsOf(text: string, organized: Record<string, Lint[]>): Flag[] {
  const flags: Flag[] = []
  for (const [rule, lints] of Object.entries(organized)) {
    const kind = rule === SPELLING ? 'spelling' : 'grammar'
    for (const lint of lints) {
      const span = lint.span()
      const at = anchored(text, span, offersOf(lint, text.slice(span.start, span.end)))
      if (at.end > at.start) flags.push({ ...at, kind })
    }
  }
  return flags.sort((a, b) => a.start - b.start || a.end - b.end)
}

async function fits(linter: LocalLinter, offer: string): Promise<boolean> {
  if (!/^an? /.test(offer)) return true
  const lints = await linter.organizedLints(offer, { language: 'plaintext' })
  return !(lints['AnA']?.length)
}

export async function lint(linter: LocalLinter, text: string): Promise<Flag[]> {
  const prose = filtered(text)
  const flags = flagsOf(prose, await linter.organizedLints(prose, { language: 'plaintext' })).filter(flag => intact(text, flag))
  for (const flag of flags) {
    const kept = []
    for (const offer of flag.offers) if (await fits(linter, offer)) kept.push(offer)
    flag.offers = kept
  }
  return flags
}
