const NOT_A_WORD = /[\/\\:#@~_0-9]/
const FILLER = '0'

function classified(line: string) {
  let quoted = false
  return (line.match(/\S+\s*|\s+/g) ?? []).map(token => {
    const word = token.trimEnd()
    const inside = quoted
    if ((word.match(/`/g) ?? []).length % 2 === 1) quoted = !quoted
    return { token, code: inside || quoted || isCode(word) }
  })
}

export function isCode(token: string): boolean {
  if (token.startsWith('-') || token.startsWith('$') || token.includes('`')) return true
  if (NOT_A_WORD.test(token)) return true
  return isDotted(token) || isDotfile(token) || isCased(token)
}

const isDotfile = (word: string) => /^\.[\p{L}\p{N}]/u.test(word)
const isDotted = (word: string) => /[\p{L}\p{N}]\.[\p{L}\p{N}]/u.test(word)
const isCased = (word: string) => /\p{Ll}\p{Lu}/u.test(word)

export function filtered(line: string): string {
  return classified(line)
    .map(({ token, code }) => (code ? token.replace(/\S/g, FILLER) : token))
    .join('')
}

function takenOut(line: string): boolean[] {
  return classified(line).flatMap(({ token, code }) => token.split('').map(c => code && !/\s/.test(c)))
}

export function intact(line: string, { start, end }: { start: number; end: number }): boolean {
  return !takenOut(line).slice(start, end).includes(true)
}
