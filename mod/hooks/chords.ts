import type { PromptEditInput } from 'claude-code'

import type { Flag } from './spans.ts'

export const FIX = ';;'
export const LEARN = ';+'

export type Box = { text: string; cursor: number }

export function chordAt(e: PromptEditInput, pair: string): Box | undefined {
  const burst = e.inputText === pair
  const at = burst ? e.start : e.start - 1
  if (!burst && (e.inputText !== pair[1] || e.start !== e.end || e.text[at] !== pair[0])) return undefined
  return { text: e.text.slice(0, at) + e.text.slice(e.end), cursor: at }
}

export function nearest(flags: Flag[], cursor: number): Flag | undefined {
  return flags.filter(flag => flag.end <= cursor).sort((a, b) => b.end - a.end)[0]
}

export type Cycle = { at: number; standing: string; original: string; offers: string[]; offered: number }

export function cycleOver(text: string, flag: Flag): Cycle | undefined {
  if (flag.offers.length === 0) return undefined
  const spaced = flag.offers.includes('') && flag.start > 0 && text[flag.start - 1] === ' '
  const at = spaced ? flag.start - 1 : flag.start
  const original = text.slice(at, flag.end)
  const offers = spaced ? flag.offers.map(offer => (offer === '' ? '' : ` ${offer}`)) : flag.offers
  return { at, standing: original, original, offers, offered: -1 }
}

export function isStanding(cycle: Cycle, text: string): boolean {
  return text.slice(cycle.at, cycle.at + cycle.standing.length) === cycle.standing
}

export function turn(cycle: Cycle, box: Box): { cycle: Cycle; box: Box } {
  const round = [...cycle.offers, cycle.original]
  const offered = (cycle.offered + 1) % round.length
  const standing = round[offered]!
  const text = box.text.slice(0, cycle.at) + standing + box.text.slice(cycle.at + cycle.standing.length)
  const after = cycle.at + cycle.standing.length
  const cursor = box.cursor >= after ? box.cursor - cycle.standing.length + standing.length : box.cursor
  return { cycle: { ...cycle, standing, offered }, box: { text, cursor } }
}
