import { test } from 'node:test'
import assert from 'node:assert/strict'

import { FIX, LEARN, chordAt, cycleOver, isStanding, nearest, turn } from '../hooks/chords.ts'

const typing = (text: string, inputText: string) => ({
  origin: { kind: 'composer' as const },
  text,
  cursor: text.length,
  start: text.length,
  end: text.length,
  inputText,
})
const spelling = (start: number, end: number, offers: string[]) => ({ start, end, kind: 'spelling' as const, offers })

test('the fix chord is the pair typed in a row, and the box loses its first half', () => {
  assert.deepEqual(chordAt(typing('fix teh bug;', ';'), FIX), { text: 'fix teh bug', cursor: 11 })
  assert.equal(chordAt(typing('fix teh bug', ';'), FIX), undefined, 'one semicolon is not a chord')
  assert.equal(chordAt(typing('fix teh bug;', 'x'), FIX), undefined)
})

test('a fast pair folded into one edit is the chord too', () => {
  assert.deepEqual(chordAt(typing('fix teh bug', ';;'), FIX), { text: 'fix teh bug', cursor: 11 })
})

test('fixing a repeated word takes the space before it along', () => {
  const box = { text: 'the the cat', cursor: 11 }
  const turned = turn(cycleOver(box.text, { start: 4, end: 7, kind: 'grammar', offers: [''] })!, box)
  assert.deepEqual(turned.box, { text: 'the cat', cursor: 7 })
  assert.deepEqual(turn(turned.cycle, turned.box).box, { text: 'the the cat', cursor: 11 })
})

test('the learn chord only counts after a space, so a namespace stays code', () => {
  assert.deepEqual(chordAt(typing('teh :', ':'), LEARN, true), { text: 'teh ', cursor: 4 })
  assert.equal(chordAt(typing('std:', ':'), LEARN, true), undefined)
  assert.deepEqual(chordAt(typing(':', ':'), LEARN, true), { text: '', cursor: 0 })
})

test('the nearest flag is the last one ending at or before the cursor', () => {
  const flags = [spelling(0, 3, ['the']), spelling(8, 11, ['bug'])]
  assert.equal(nearest(flags, 11)?.start, 8)
  assert.equal(nearest(flags, 10)?.start, 0)
  assert.equal(nearest(flags, 2), undefined)
})

test('the fix chord cycles through the offers and back to the original', () => {
  const box = { text: 'fix teh bug', cursor: 11 }
  let cycle = cycleOver(box.text, spelling(4, 7, ['the', 'tea']))!
  let turned = turn(cycle, box)
  assert.deepEqual(turned.box, { text: 'fix the bug', cursor: 11 })
  assert.ok(isStanding(turned.cycle, turned.box.text))
  turned = turn(turned.cycle, turned.box)
  assert.deepEqual(turned.box, { text: 'fix tea bug', cursor: 11 })
  turned = turn(turned.cycle, turned.box)
  assert.deepEqual(turned.box, { text: 'fix teh bug', cursor: 11 })
})

test('a longer offer moves a cursor that sat after the word', () => {
  const box = { text: 'send report now', cursor: 15 }
  const turned = turn(cycleOver(box.text, { start: 5, end: 11, kind: 'grammar', offers: ['a report'] })!, box)
  assert.deepEqual(turned.box, { text: 'send a report now', cursor: 17 })
})

test('a flag without offers starts no cycle', () => {
  assert.equal(cycleOver('fix teh', spelling(4, 7, [])), undefined)
})

test('an edit elsewhere ends the cycle', () => {
  const cycle = turn(cycleOver('fix teh bug', spelling(4, 7, ['the']))!, { text: 'fix teh bug', cursor: 11 }).cycle
  assert.ok(!isStanding(cycle, 'fix tha bug'))
})
