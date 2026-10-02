import { test } from 'node:test'
import assert from 'node:assert/strict'

import { decorationsOf, isBeingTyped } from '../hooks/spans.ts'

const flag = (start: number, end: number) => ({ start, end, kind: 'spelling' as const, offers: [] })
const red = (start: number, end: number) => ({ start, end, underline: true, color: 'red' })
const AWAY = 100

test('a span is painted where it is given, in UTF-16 units', () => {
  assert.deepEqual(decorationsOf('fix teh bug', [flag(4, 7)], AWAY), [red(4, 7)])
  assert.deepEqual(decorationsOf('🙂 teh', [flag(3, 6)], AWAY), [red(3, 6)])
})

test('a grammar flag paints blue', () => {
  assert.deepEqual(decorationsOf('its fine', [{ start: 0, end: 3, kind: 'grammar', offers: [] }], AWAY), [
    { start: 0, end: 3, underline: true, color: 'blueBright' },
  ])
})

test('the word being typed is not flagged until the cursor leaves it', () => {
  assert.deepEqual(decorationsOf('teh', [flag(0, 3)], 3), [], 'cursor at the end of the word')
  assert.deepEqual(decorationsOf('teh ', [flag(0, 3)], 4), [red(0, 3)], 'a space finished it')
  assert.deepEqual(decorationsOf('teh ', [flag(0, 3)], 2), [], 'cursor moved back inside')
})

test('the cursor at each edge of a word says whether it is finished', () => {
  const held = (text: string, cursor: number) => isBeingTyped(text, cursor, 2, 5)
  assert.equal(held('a teh', 2), false, 'in front of the word')
  assert.equal(held('a teh', 3), true, 'inside the word')
  assert.equal(held('a teh', 5), true, 'the word ends where the typing stopped')
  assert.equal(held('a tehx', 5), true, 'more of the word follows the cursor')
  assert.equal(held('a teh word', 5), false, 'finished and spaced')
  assert.equal(held('a teh, x', 5), false, 'finished and punctuated')
  assert.equal(held('a teh word', 10), false, 'the cursor has moved on')
})

test('a word character after the cursor is read whole, not as a lone surrogate', () => {
  assert.equal(isBeingTyped('a teh𝐀', 5, 2, 5), true)
})
